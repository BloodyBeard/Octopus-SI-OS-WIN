#!/usr/bin/env python3
import atexit
import base64
import http.server
import http.client
import hashlib
import hmac
import ipaddress
import json
import math
import mimetypes
import os
import re
import secrets
import signal
import socket
import sqlite3
import ssl
import stat
import struct
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from octopus_content_hash import octopus_content_hash

from telegram_rendering import legacy_chunks
from telegram_rendering import needs_rich_rendering
from telegram_rendering import rich_message_payload
from telegram_rendering import rich_safe
from telegram_rendering import text_units

INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.environ["OCTOPUS_INTERFACE_DIR"]
VAR_DIR = os.environ["OCTOPUS_VAR_DIR"]
MOVEMENTS_JSONL = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
BLOBS_DIR = os.environ["OCTOPUS_BLOBS_DIR"]
LEDGER_INDEX_SQLITE = os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]

CONFIG_PATH = os.path.join(INTERFACE_DIR, "config.json")
CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")
CHAT_STATES_DIR = os.path.join(VAR_DIR, "chats")
INTAKE_CURSOR_PATH = os.path.join(VAR_DIR, "telegram-update-offset.json")
PAYLOADS_DIR = os.path.join(VAR_DIR, "payloads")
MEDIA_DIR = os.path.join(VAR_DIR, "media")
PENDING_INTAKE_DIR = os.path.join(VAR_DIR, "pending-intake")
PAIRING_PATH = os.path.join(VAR_DIR, "pairing.json")
EFFECTS_DIR = os.path.join(VAR_DIR, "outbound-effects")
BINDINGS_PATH = os.path.join(VAR_DIR, "bindings.json")
TOPOLOGY_OWNERSHIP_PATH = os.path.join(VAR_DIR, "topology-ownership.json")
IDENTITY_SALT_PATH = os.path.join(VAR_DIR, "identity-salt")
BINDING_SECRETS_DIR = os.path.join(VAR_DIR, "binding-secrets")
PAIRING_TTL_MS = 10 * 60 * 1000
PAIRING_MAX_PENDING = 1024
PAIRING_MAX_APPROVED = 100000
PAIRING_MAX_STATE_BYTES = 2 * 1024 * 1024
TOPOLOGY_MAX_BYTES = 16 * 1024 * 1024
TOPOLOGY_OWNERSHIP_MAX_PATHS = 100_000
PRIVATE_REGISTRATION_LIMIT = 32
PRIVATE_REGISTRATION_MAX_BYTES = 16 * 1024
os.umask(0o077)


def _require_private_mode(status, expected, name):
    if os.name == "posix" and stat.S_IMODE(status.st_mode) != expected:
        raise PermissionError(f"{name} must have mode {expected:04o}")


def _ensure_private_directory(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    status = os.lstat(path)
    if stat.S_ISLNK(status.st_mode) or not stat.S_ISDIR(status.st_mode):
        raise PermissionError(f"private directory is not a real directory: {path}")
    _require_private_mode(status, 0o700, path)
    return path


def _open_private_file(path, flags, create=False):
    before = None
    try:
        before = os.lstat(path)
        if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
            raise PermissionError(f"private path is not a regular file: {path}")
        _require_private_mode(before, 0o600, path)
    except FileNotFoundError:
        if not create:
            raise
    descriptor = os.open(path, flags | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        current = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(current.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode):
            raise PermissionError(f"private path is not a regular file: {path}")
        _require_private_mode(current, 0o600, path)
        if (linked.st_dev, linked.st_ino) != (current.st_dev, current.st_ino):
            raise PermissionError(f"private path changed while opening: {path}")
        if before is not None and (before.st_dev, before.st_ino) != (current.st_dev, current.st_ino):
            raise PermissionError(f"private path changed while opening: {path}")
        return descriptor
    except Exception:
        os.close(descriptor)
        raise


def _open_private_read(path):
    return os.fdopen(_open_private_file(path, os.O_RDONLY), "rb")


class TelegramMediaTooLarge(ValueError):
    def __init__(self, maximum):
        self.maximum = maximum
        super().__init__("telegram media exceeds configured byte ceiling")


def _read_rooted_regular(root, path, maximum):
    root = os.path.realpath(root)
    path = os.path.realpath(path)
    if os.path.commonpath((path, root)) != root or path == root:
        raise ValueError("telegram local Bot API file path escapes configured roots")
    relative = os.path.relpath(path, root)
    components = relative.split(os.sep)
    if not components or any(component in ("", ".", "..") for component in components):
        raise ValueError("telegram local Bot API file path is malformed")
    directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptors = [os.open(root, directory_flags)]
    file_descriptor = None
    try:
        for component in components[:-1]:
            descriptors.append(os.open(component, directory_flags, dir_fd=descriptors[-1]))
        file_descriptor = os.open(components[-1], os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0), dir_fd=descriptors[-1])
        status = os.fstat(file_descriptor)
        if not stat.S_ISREG(status.st_mode):
            raise ValueError("telegram local Bot API file path is not a regular file")
        result = bytearray()
        while len(result) <= maximum:
            chunk = os.read(file_descriptor, min(65536, maximum + 1 - len(result)))
            if not chunk:
                break
            result.extend(chunk)
        if len(result) > maximum:
            raise TelegramMediaTooLarge(maximum)
        return bytes(result)
    finally:
        if file_descriptor is not None:
            os.close(file_descriptor)
        for descriptor in reversed(descriptors):
            os.close(descriptor)

for _dir in (
    VAR_DIR,
    CHAT_STATES_DIR,
    PAYLOADS_DIR,
    MEDIA_DIR,
    EFFECTS_DIR,
    PENDING_INTAKE_DIR,
    BINDING_SECRETS_DIR,
):
    _ensure_private_directory(_dir)

_STDOUT_LOCK = threading.Lock()
_VERDICT_CONDITION = threading.Condition()
_VERDICTS = {}
_VERDICT_INPUT_ERROR = None
_NEXT_REQUEST_ID = 1
_STATE_LOCK = threading.RLock()
_AUTHORING_LOCK = threading.RLock()
_LEDGER_LOCK = threading.RLock()
_LEDGER_FALLBACK_OFFSET = 0
_LEDGER_FALLBACK_SIZE = 0
_LEDGER_FALLBACK_IDENTITY = None
_LEDGER_INDEX_WARNING = None
_RESOLVED_BOT_USERNAME = None
_RICH_SEND_DISABLED = False
_RICH_DRAFT_DISABLED = False
_INTAKE_BATCHER = None
_ACTIVE_BOT_TOKEN = ""
_ACTIVE_PROXY_URL = ""


def load_config():
    config = json.loads(
        _read_bounded_regular(CONFIG_PATH, TOPOLOGY_MAX_BYTES, "interface config")
    )
    _validate_config(config)
    return config


def _require_object(value, name):
    if not isinstance(value, dict):
        raise ValueError(f"{name} must be a JSON object")
    return value


def _require_keys(value, required, optional, name):
    obj = _require_object(value, name)
    missing = sorted(set(required) - set(obj))
    unknown = sorted(set(obj) - set(required) - set(optional))
    if missing:
        raise ValueError(f"{name} is missing keys: {', '.join(missing)}")
    if unknown:
        raise ValueError(f"{name} has unknown keys: {', '.join(unknown)}")
    return obj


def _require_string(value, name):
    if not isinstance(value, str) or not value or value.strip() != value:
        raise ValueError(f"{name} must be a non-empty trimmed string")
    return value


def _require_bool(value, name):
    if not isinstance(value, bool):
        raise ValueError(f"{name} must be a boolean")
    return value


def _require_int(value, name, minimum=None, maximum=None):
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"{name} must be an integer")
    if minimum is not None and value < minimum:
        raise ValueError(f"{name} must be at least {minimum}")
    if maximum is not None and value > maximum:
        raise ValueError(f"{name} must be at most {maximum}")
    return value


def _require_string_list(value, name, allow_empty=False):
    if not isinstance(value, list) or (not allow_empty and not value):
        raise ValueError(f"{name} must be a{' possibly empty' if allow_empty else ' non-empty'} array")
    seen = set()
    items = []
    for index, item in enumerate(value):
        item = _require_string(item, f"{name}[{index}]")
        if item in seen:
            raise ValueError(f"{name} repeats {item!r}")
        seen.add(item)
        items.append(item)
    return items


def _authored_interface_binding(interface):
    return {
        "interface": _require_string(interface, "interface binding identity"),
        "config": {"artifact_policy": {"mode": "all"}},
    }


def _require_interface_binding(value, name):
    binding = _require_keys(value, {"interface", "config"}, set(), name)
    interface = _require_string(binding["interface"], f"{name}.interface")
    config = _require_keys(binding["config"], {"artifact_policy"}, set(), f"{name}.config")
    policy = _require_object(config["artifact_policy"], f"{name}.config.artifact_policy")
    mode = _require_string(policy.get("mode"), f"{name}.config.artifact_policy.mode")
    if mode == "all":
        _require_keys(policy, {"mode"}, set(), f"{name}.config.artifact_policy")
    elif mode == "allow":
        _require_keys(policy, {"mode", "names", "namespaces"}, set(), f"{name}.config.artifact_policy")
        names = _require_string_list(policy["names"], f"{name}.config.artifact_policy.names", True)
        namespaces = _require_string_list(policy["namespaces"], f"{name}.config.artifact_policy.namespaces", True)
        if not names and not namespaces:
            raise ValueError(f"{name}.config.artifact_policy allow mode grants nothing")
        if any(namespace.endswith(".") for namespace in namespaces):
            raise ValueError(f"{name}.config.artifact_policy namespace must not end with '.'")
    else:
        raise ValueError(f"{name}.config.artifact_policy.mode must be all or allow")
    return interface


def _validate_config(config):
    _require_keys(
        config,
        {
            "schema_version",
            "mode",
            "bot_api",
            "intake",
            "authz",
            "gating",
            "media",
            "outbound",
            "driver_ipc",
            "proactive_bindings",
            "private_run",
        },
        {"binding", "gateway_run_template"},
        "telegram config",
    )
    if config["schema_version"] != 2:
        raise ValueError("telegram config schema_version must be 2")
    if config["mode"] not in ("gateway", "binding"):
        raise ValueError("telegram config mode must be 'gateway' or 'binding'")
    if config["mode"] == "gateway" and "binding" in config:
        raise ValueError("telegram gateway config must not declare binding coordinates")
    if config["mode"] == "gateway":
        template = _require_object(
            config.get("gateway_run_template"), "telegram gateway_run_template"
        )
        _require_string(template.get("id"), "telegram gateway_run_template.id")
        _require_string(template.get("ee"), "telegram gateway_run_template.ee")
        _require_string(template.get("worker"), "telegram gateway_run_template.worker")
    elif "gateway_run_template" in config:
        raise ValueError("telegram binding config must not declare gateway_run_template")
    if config["mode"] == "binding":
        binding = _require_keys(
            config.get("binding"),
            {
                "interface",
                "chat_id",
                "thread_id",
                "gateway_interface",
                "secret_handle",
                "host",
                "port",
                "run",
            },
            set(),
            "telegram binding",
        )
        if _require_string(binding["interface"], "telegram binding.interface") != INTERFACE_ID:
            raise ValueError("telegram binding.interface must match the interface folder identity")
        validate_chat_coordinates(binding["chat_id"], binding["thread_id"])
        _require_string(binding["gateway_interface"], "telegram binding.gateway_interface")
        _require_string(binding["run"], "telegram binding.run")
        secret_handle = _require_string(binding["secret_handle"], "telegram binding.secret_handle")
        if not os.path.isabs(secret_handle):
            raise ValueError("telegram binding.secret_handle must be absolute")
        binding_host = _require_string(binding["host"], "telegram binding.host")
        if not _is_loopback_host(binding_host):
            raise ValueError("telegram binding.host must be loopback")
        _require_int(binding["port"], "telegram binding.port", 1, 65535)
    bot_api = _require_keys(
        config["bot_api"],
        {
            "base_url_env",
            "base_url_default",
            "token_env",
            "request_timeout_ms",
            "max_response_bytes",
            "proxy_env",
            "local_mode_env",
            "local_file_roots_env",
            "local_file_root_slots",
            "fallback_ips_env",
            "seed_fallback_ips",
            "fallback_discovery",
        },
        set(),
        "telegram bot_api",
    )
    for key in (
        "base_url_env",
        "base_url_default",
        "token_env",
        "proxy_env",
        "local_mode_env",
        "local_file_roots_env",
        "fallback_ips_env",
    ):
        _require_string(bot_api[key], f"telegram bot_api.{key}")
    _require_int(bot_api["request_timeout_ms"], "telegram bot_api.request_timeout_ms", 1000, 600000)
    _require_int(bot_api["max_response_bytes"], "telegram bot_api.max_response_bytes", 1024, 67108864)
    _require_string_list(bot_api["seed_fallback_ips"], "telegram bot_api.seed_fallback_ips", True)
    _require_string_list(
        bot_api["local_file_root_slots"],
        "telegram bot_api.local_file_root_slots",
        True,
    )
    discovery = bot_api["fallback_discovery"]
    if not isinstance(discovery, list):
        raise ValueError("telegram bot_api.fallback_discovery must be an array")
    for index, provider in enumerate(discovery):
        provider = _require_keys(
            provider,
            {
                "url",
                "answers_path",
                "address_key",
                "record_type_key",
                "record_type_value",
            },
            {"headers"},
            f"telegram bot_api.fallback_discovery[{index}]",
        )
        url = _require_string(
            provider["url"], f"telegram bot_api.fallback_discovery[{index}].url"
        )
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme != "https" or not parsed.hostname:
            raise ValueError("telegram fallback discovery URLs must be absolute HTTPS URLs")
        _require_string_list(
            provider["answers_path"],
            f"telegram bot_api.fallback_discovery[{index}].answers_path",
        )
        _require_string(
            provider["address_key"],
            f"telegram bot_api.fallback_discovery[{index}].address_key",
        )
        _require_string(
            provider["record_type_key"],
            f"telegram bot_api.fallback_discovery[{index}].record_type_key",
        )
        _require_int(
            provider["record_type_value"],
            f"telegram bot_api.fallback_discovery[{index}].record_type_value",
            0,
        )
        headers = provider.get("headers", {})
        if not isinstance(headers, dict) or any(
            not isinstance(key, str) or not isinstance(value, str)
            for key, value in headers.items()
        ):
            raise ValueError(
                f"telegram bot_api.fallback_discovery[{index}].headers must be a string map"
            )
    intake = _require_keys(
        config["intake"],
        {
            "mode",
            "long_poll_timeout_s",
            "allowed_updates",
            "drop_pending_updates",
            "conflict_backoff_ms",
            "conflict_max_retries",
            "network_backoff_ms",
            "network_backoff_cap_ms",
            "batching",
            "webhook",
        },
        set(),
        "telegram intake",
    )
    if intake.get("mode") not in ("long_poll", "webhook"):
        raise ValueError("telegram intake.mode must be 'long_poll' or 'webhook'")
    _require_int(intake.get("long_poll_timeout_s"), "telegram intake.long_poll_timeout_s", 1, 50)
    _require_string_list(intake.get("allowed_updates"), "telegram intake.allowed_updates")
    for key in ("drop_pending_updates",):
        _require_bool(intake.get(key), f"telegram intake.{key}")
    for key in (
        "conflict_backoff_ms",
        "conflict_max_retries",
        "network_backoff_ms",
        "network_backoff_cap_ms",
    ):
        _require_int(intake.get(key), f"telegram intake.{key}", 0, 600000)
    batching = _require_keys(
        intake.get("batching"),
        {
            "enabled",
            "text_quiet_ms",
            "text_split_quiet_ms",
            "split_threshold_units",
            "media_group_quiet_ms",
            "photo_burst_quiet_ms",
            "max_items",
            "max_text_chars",
        },
        set(),
        "telegram intake.batching",
    )
    _require_bool(batching.get("enabled"), "telegram intake.batching.enabled")
    for key in (
        "text_quiet_ms",
        "text_split_quiet_ms",
        "media_group_quiet_ms",
        "photo_burst_quiet_ms",
    ):
        _require_int(batching.get(key), f"telegram intake.batching.{key}", 0, 10000)
    _require_int(
        batching.get("split_threshold_units"),
        "telegram intake.batching.split_threshold_units",
        1,
        4096,
    )
    _require_int(batching.get("max_items"), "telegram intake.batching.max_items", 1, 100)
    _require_int(
        batching.get("max_text_chars"),
        "telegram intake.batching.max_text_chars",
        1,
        1048576,
    )
    webhook = _require_keys(
        intake.get("webhook"),
        {"enabled_env", "secret_env", "listen", "port", "port_env", "path", "max_body_bytes"},
        set(),
        "telegram intake.webhook",
    )
    for key in ("enabled_env", "secret_env", "listen", "port_env", "path"):
        _require_string(webhook.get(key), f"telegram intake.webhook.{key}")
    _require_int(webhook.get("port"), "telegram intake.webhook.port", 1, 65535)
    _require_int(webhook.get("max_body_bytes"), "telegram intake.webhook.max_body_bytes", 1, 16777216)
    authz = _require_keys(
        config["authz"],
        {
            "allow_all_users",
            "allow_all_users_env",
            "allowed_user_ids",
            "allowed_user_ids_env",
            "allowed_chat_ids",
            "allowed_chat_ids_env",
            "deny_unmatched",
            "pairing",
        },
        set(),
        "telegram authz",
    )
    _require_bool(authz.get("allow_all_users"), "telegram authz.allow_all_users")
    _require_string(authz.get("allow_all_users_env"), "telegram authz.allow_all_users_env")
    for key in ("allowed_user_ids", "allowed_chat_ids"):
        values = authz.get(key)
        if not isinstance(values, list) or any(isinstance(item, bool) or not isinstance(item, int) for item in values):
            raise ValueError(f"telegram authz.{key} must be an integer array")
    for key in ("allowed_user_ids_env", "allowed_chat_ids_env"):
        _require_string(authz.get(key), f"telegram authz.{key}")
    _require_bool(authz.get("deny_unmatched"), "telegram authz.deny_unmatched")
    pairing = _require_keys(
        authz.get("pairing"),
        {"enabled", "prompt"},
        set(),
        "telegram authz.pairing",
    )
    _require_bool(pairing.get("enabled"), "telegram authz.pairing.enabled")
    _require_string(pairing.get("prompt"), "telegram authz.pairing.prompt")
    gating = _require_keys(
        config["gating"],
        {
            "respond_in_private",
            "respond_in_groups_on_mention_only",
            "bot_username_env",
            "mention_aliases",
            "observe_unmentioned_group_messages",
            "reply_to_bot_counts_as_mention",
            "group_allowed_user_ids",
            "group_allowed_user_ids_env",
            "group_allowed_chat_ids",
            "group_allowed_chat_ids_env",
            "free_response_chat_ids",
            "free_response_chat_ids_env",
            "observe_chat_ids",
            "observe_chat_ids_env",
            "allowed_topics",
            "ignored_topics",
            "mention_patterns",
            "exclusive_bot_mentions",
            "guest_mode",
        },
        set(),
        "telegram gating",
    )
    for key in (
        "respond_in_private",
        "respond_in_groups_on_mention_only",
        "observe_unmentioned_group_messages",
        "reply_to_bot_counts_as_mention",
        "exclusive_bot_mentions",
        "guest_mode",
    ):
        _require_bool(gating.get(key), f"telegram gating.{key}")
    _require_string(gating.get("bot_username_env"), "telegram gating.bot_username_env")
    _require_string_list(gating.get("mention_aliases"), "telegram gating.mention_aliases", True)
    for key in (
        "group_allowed_user_ids",
        "group_allowed_chat_ids",
        "free_response_chat_ids",
        "observe_chat_ids",
    ):
        values = gating.get(key)
        if not isinstance(values, list) or any(
            isinstance(item, bool) or not isinstance(item, int) for item in values
        ):
            raise ValueError(f"telegram gating.{key} must be an integer array")
    for key in ("allowed_topics", "ignored_topics"):
        topics = gating.get(key)
        if not isinstance(topics, list):
            raise ValueError(f"telegram gating.{key} must be an array")
        seen_topics = set()
        for index, topic in enumerate(topics):
            topic = _require_keys(
                topic,
                {"chat_id", "thread_id"},
                set(),
                f"telegram gating.{key}[{index}]",
            )
            validate_chat_coordinates(topic["chat_id"], topic["thread_id"])
            if topic["thread_id"] is None:
                raise ValueError(f"telegram gating.{key}[{index}].thread_id must be present")
            coordinate = (topic["chat_id"], topic["thread_id"])
            if coordinate in seen_topics:
                raise ValueError(f"telegram gating.{key} repeats a topic coordinate")
            seen_topics.add(coordinate)
    for key in (
        "group_allowed_user_ids_env",
        "group_allowed_chat_ids_env",
        "free_response_chat_ids_env",
        "observe_chat_ids_env",
    ):
        _require_string(gating.get(key), f"telegram gating.{key}")
    patterns = _require_string_list(
        gating.get("mention_patterns"), "telegram gating.mention_patterns", True
    )
    for pattern in patterns:
        try:
            re.compile(pattern, re.IGNORECASE)
        except re.error as error:
            raise ValueError(f"telegram mention pattern {pattern!r} is invalid: {error}") from error
    media = _require_keys(
        config["media"],
        {
            "max_download_bytes",
            "max_upload_bytes",
            "max_multipart_bytes",
            "max_text_preview_bytes",
            "max_media_group_items",
            "text_extensions",
            "inbound_kinds",
        },
        set(),
        "telegram media",
    )
    for key in ("max_download_bytes", "max_upload_bytes"):
        _require_int(media.get(key), f"telegram media.{key}", 1, 536870912)
    _require_int(media.get("max_multipart_bytes"), "telegram media.max_multipart_bytes", 1, 536870912)
    _require_int(
        media.get("max_text_preview_bytes"),
        "telegram media.max_text_preview_bytes",
        0,
        1048576,
    )
    _require_int(media.get("max_media_group_items"), "telegram media.max_media_group_items", 2, 10)
    text_extensions = _require_string_list(
        media.get("text_extensions"), "telegram media.text_extensions", True
    )
    if any(not re.fullmatch(r"\.[a-z0-9][a-z0-9._+-]{0,15}", ext) for ext in text_extensions):
        raise ValueError("telegram media.text_extensions contains a malformed extension")
    inbound_kinds = _require_string_list(
        media.get("inbound_kinds"), "telegram media.inbound_kinds", True
    )
    supported_media = {"photo", "voice", "audio", "video", "video_note", "animation", "document", "sticker"}
    unsupported_media = sorted(set(inbound_kinds) - supported_media)
    if unsupported_media:
        raise ValueError(f"telegram media.inbound_kinds contains unsupported values: {', '.join(unsupported_media)}")
    outbound = _require_keys(
        config["outbound"],
        {
            "parse_mode",
            "max_message_len",
            "max_caption_len",
            "disable_link_previews",
            "min_edit_interval_ms",
            "flood_backoff_ms",
            "flood_max_wait_ms",
            "flood_max_retries",
            "silent_default",
            "reply_labels",
            "stream_labels",
            "error_labels",
            "turn_start_labels",
            "telemetry_labels",
            "allowed_api_methods",
            "api_method_options",
            "commands",
            "reactions",
            "typing",
            "rich_messages",
            "streaming",
            "status_indicator",
        },
        set(),
        "telegram outbound",
    )
    if _require_string(outbound.get("parse_mode"), "telegram outbound.parse_mode") != "MarkdownV2":
        raise ValueError("telegram outbound.parse_mode must be MarkdownV2")
    _require_int(outbound.get("max_message_len"), "telegram outbound.max_message_len", 1, 4096)
    _require_int(outbound.get("max_caption_len"), "telegram outbound.max_caption_len", 1, 1024)
    _require_int(outbound.get("min_edit_interval_ms"), "telegram outbound.min_edit_interval_ms", 0, 60000)
    _require_int(outbound.get("flood_backoff_ms"), "telegram outbound.flood_backoff_ms", 0, 600000)
    _require_int(outbound.get("flood_max_wait_ms"), "telegram outbound.flood_max_wait_ms", 0, 600000)
    _require_int(outbound.get("flood_max_retries"), "telegram outbound.flood_max_retries", 0, 20)
    if outbound["flood_backoff_ms"] > outbound["flood_max_wait_ms"]:
        raise ValueError("telegram outbound.flood_backoff_ms exceeds flood_max_wait_ms")
    for key in ("disable_link_previews", "silent_default"):
        _require_bool(outbound.get(key), f"telegram outbound.{key}")
    label_sets = []
    for key in (
        "reply_labels",
        "stream_labels",
        "error_labels",
        "turn_start_labels",
        "telemetry_labels",
    ):
        labels = _require_string_list(outbound.get(key), f"telegram outbound.{key}", True)
        label_sets.extend(labels)
    if len(label_sets) != len(set(label_sets)):
        raise ValueError("telegram outbound label sets overlap")
    _require_string_list(outbound.get("allowed_api_methods"), "telegram outbound.allowed_api_methods", True)
    method_options = outbound.get("api_method_options")
    if not isinstance(method_options, dict):
        raise ValueError("telegram outbound.api_method_options must be an object")
    allowed_methods = set(outbound["allowed_api_methods"])
    unknown_options = sorted(set(method_options) - allowed_methods)
    if unknown_options:
        raise ValueError(
            "telegram outbound.api_method_options names undeclared methods: "
            + ", ".join(unknown_options)
        )
    for method, options in method_options.items():
        options = _require_keys(
            options,
            {"inject_chat_id", "inject_thread_id"},
            set(),
            f"telegram outbound.api_method_options.{method}",
        )
        _require_bool(
            options["inject_chat_id"],
            f"telegram outbound.api_method_options.{method}.inject_chat_id",
        )
        if options["inject_chat_id"] is not True:
            raise ValueError(
                f"telegram outbound.api_method_options.{method} cannot escape the bound chat"
            )
        _require_bool(
            options["inject_thread_id"],
            f"telegram outbound.api_method_options.{method}.inject_thread_id",
        )
    commands = _require_keys(
        outbound.get("commands"),
        {"auto_register", "menu", "scopes"},
        set(),
        "telegram outbound.commands",
    )
    _require_bool(commands.get("auto_register"), "telegram outbound.commands.auto_register")
    menu = commands.get("menu")
    if not isinstance(menu, list) or len(menu) > 100:
        raise ValueError("telegram outbound.commands.menu must be an array of at most 100 entries")
    for index, item in enumerate(menu):
        command = _require_keys(
            item,
            {"command", "description"},
            set(),
            f"telegram outbound.commands.menu[{index}]",
        )
        name = _require_string(command["command"], f"telegram outbound.commands.menu[{index}].command")
        if not re.fullmatch(r"[a-z0-9_]{1,32}", name):
            raise ValueError(f"telegram outbound command {name!r} is malformed")
        description = _require_string(
            command["description"], f"telegram outbound.commands.menu[{index}].description"
        )
        if len(description) > 256:
            raise ValueError(f"telegram outbound command {name!r} description is too long")
    scopes = commands.get("scopes")
    if not isinstance(scopes, list) or not scopes:
        raise ValueError("telegram outbound.commands.scopes must be a non-empty array")
    scope_types = {
        "default",
        "all_private_chats",
        "all_group_chats",
        "all_chat_administrators",
        "chat",
        "chat_administrators",
        "chat_member",
    }
    for index, scope in enumerate(scopes):
        scope = _require_keys(
            scope,
            {"type"},
            {"chat_id", "user_id", "language_code"},
            f"telegram outbound.commands.scopes[{index}]",
        )
        scope_type = _require_string(
            scope["type"], f"telegram outbound.commands.scopes[{index}].type"
        )
        if scope_type not in scope_types:
            raise ValueError(f"telegram command scope type {scope_type!r} is unsupported")
        if scope_type in {"chat", "chat_administrators", "chat_member"}:
            _require_int(
                scope.get("chat_id"),
                f"telegram outbound.commands.scopes[{index}].chat_id",
            )
        if scope_type == "chat_member":
            _require_int(
                scope.get("user_id"),
                f"telegram outbound.commands.scopes[{index}].user_id",
                1,
            )
        if scope.get("language_code") is not None:
            language = _require_string(
                scope["language_code"],
                f"telegram outbound.commands.scopes[{index}].language_code",
            )
            if len(language) > 16:
                raise ValueError("telegram command scope language_code is too long")
    reactions = _require_keys(
        outbound.get("reactions"),
        {"enabled", "in_progress", "success", "failure", "clear_on_cancel"},
        set(),
        "telegram outbound.reactions",
    )
    _require_bool(reactions.get("enabled"), "telegram outbound.reactions.enabled")
    _require_bool(reactions.get("clear_on_cancel"), "telegram outbound.reactions.clear_on_cancel")
    for key in ("in_progress", "success", "failure"):
        _require_string(reactions.get(key), f"telegram outbound.reactions.{key}")
    typing = _require_keys(
        outbound.get("typing"),
        {"enabled", "action"},
        set(),
        "telegram outbound.typing",
    )
    _require_bool(typing.get("enabled"), "telegram outbound.typing.enabled")
    _require_string(typing.get("action"), "telegram outbound.typing.action")
    rich = _require_keys(
        outbound.get("rich_messages"),
        {
            "enabled",
            "drafts_enabled",
            "pretty_tables",
            "max_chars",
            "skip_entity_detection",
            "reject_details_math",
            "reject_cjk",
        },
        set(),
        "telegram outbound.rich_messages",
    )
    for key in (
        "enabled",
        "drafts_enabled",
        "pretty_tables",
        "skip_entity_detection",
        "reject_details_math",
        "reject_cjk",
    ):
        _require_bool(rich.get(key), f"telegram outbound.rich_messages.{key}")
    _require_int(
        rich.get("max_chars"), "telegram outbound.rich_messages.max_chars", 1, 32768
    )
    streaming = _require_keys(
        outbound.get("streaming"),
        {"transport", "native_drafts_private_only"},
        set(),
        "telegram outbound.streaming",
    )
    if streaming.get("transport") not in ("auto", "edit", "draft"):
        raise ValueError("telegram outbound.streaming.transport must be auto, edit, or draft")
    _require_bool(
        streaming.get("native_drafts_private_only"),
        "telegram outbound.streaming.native_drafts_private_only",
    )
    status_indicator = _require_keys(
        outbound.get("status_indicator"),
        {"enabled", "online_text", "offline_text"},
        set(),
        "telegram outbound.status_indicator",
    )
    _require_bool(
        status_indicator.get("enabled"), "telegram outbound.status_indicator.enabled"
    )
    for key in ("online_text", "offline_text"):
        value = _require_string(
            status_indicator.get(key), f"telegram outbound.status_indicator.{key}"
        )
        if len(value) > 120:
            raise ValueError(f"telegram outbound.status_indicator.{key} exceeds 120 characters")
    ipc = _require_keys(
        config["driver_ipc"],
        {
            "listen",
            "port",
            "state_file",
            "connect_timeout_ms",
            "request_timeout_ms",
            "reconnect_backoff_ms",
            "reconnect_backoff_cap_ms",
            "max_manifest_bytes",
            "max_frame_bytes",
        },
        set(),
        "telegram driver_ipc",
    )
    ipc_host = _require_string(ipc.get("listen"), "telegram driver_ipc.listen")
    try:
        if not ipaddress.ip_address(ipc_host).is_loopback:
            raise ValueError("telegram driver_ipc.listen must be a loopback IP address")
    except ValueError as error:
        if str(error).startswith("telegram "):
            raise
        raise ValueError("telegram driver_ipc.listen must be a loopback IP address") from error
    _require_string(ipc.get("state_file"), "telegram driver_ipc.state_file")
    _require_int(ipc.get("port"), "telegram driver_ipc.port", 0, 65535)
    for key in (
        "connect_timeout_ms",
        "request_timeout_ms",
        "reconnect_backoff_ms",
        "reconnect_backoff_cap_ms",
        "max_manifest_bytes",
        "max_frame_bytes",
    ):
        _require_int(ipc.get(key), f"telegram driver_ipc.{key}", 1, 600000000)
    proactive = config["proactive_bindings"]
    if not isinstance(proactive, list):
        raise ValueError("telegram proactive_bindings must be an array")
    seen_proactive = set()
    for index, item in enumerate(proactive):
        item = _require_keys(
            item,
            {"chat_id", "thread_id"},
            set(),
            f"telegram proactive_bindings[{index}]",
        )
        validate_chat_coordinates(item["chat_id"], item["thread_id"])
        coordinate = (item["chat_id"], item["thread_id"])
        if coordinate in seen_proactive:
            raise ValueError("telegram proactive_bindings repeats a chat coordinate")
        seen_proactive.add(coordinate)
    private_run = _require_keys(
        config["private_run"],
        {
            "ee_template",
            "parent_ee",
            "model",
            "reasoning_effort",
            "role_artifacts",
            "context_routes",
            "owners",
            "id_prefixes",
            "file_write_tool",
            "file_write_label",
            "file_write_result_label",
            "worker_environment_kind",
            "worker_environment_door_owner",
            "gateway_worker_environment_interface",
            "gateway_file_write_tool",
            "file_write_mark_outcome",
            "provider_context_output_labels",
            "authoring_max_attempts",
            "result_timeout_ms",
        },
        set(),
        "telegram private_run",
    )
    for key in (
        "ee_template",
        "parent_ee",
        "model",
        "reasoning_effort",
        "file_write_tool",
        "file_write_label",
        "file_write_result_label",
        "worker_environment_kind",
        "worker_environment_door_owner",
        "gateway_worker_environment_interface",
        "gateway_file_write_tool",
        "file_write_mark_outcome",
    ):
        _require_string(private_run.get(key), f"telegram private_run.{key}")
    _require_string_list(private_run.get("role_artifacts"), "telegram private_run.role_artifacts")
    routes = private_run.get("context_routes")
    if not isinstance(routes, list):
        raise ValueError("telegram private_run.context_routes must be an array")
    for index, route in enumerate(routes):
        route = _require_keys(
            route,
            {"chat_ids", "thread_ids", "artifact_bindings"},
            set(),
            f"telegram private_run.context_routes[{index}]",
        )
        for key in ("chat_ids", "thread_ids"):
            values = route[key]
            if not isinstance(values, list) or any(
                isinstance(item, bool) or not isinstance(item, int) for item in values
            ):
                raise ValueError(
                    f"telegram private_run.context_routes[{index}].{key} must be an integer array"
                )
        _require_string_list(
            route["artifact_bindings"],
            f"telegram private_run.context_routes[{index}].artifact_bindings",
        )
    _require_string_list(
        private_run.get("provider_context_output_labels"),
        "telegram private_run.provider_context_output_labels",
    )
    owners = _require_keys(
        private_run.get("owners"),
        {"telegram", "provider", "interface_authoring", "worker"},
        set(),
        "telegram private_run.owners",
    )
    prefixes = _require_keys(
        private_run.get("id_prefixes"),
        {
            "run",
            "ee",
            "ee_template",
            "telegram",
            "provider",
            "interface_authoring",
            "worker",
            "worker_environment",
            "file_write_tool",
        },
        set(),
        "telegram private_run.id_prefixes",
    )
    for key in ("telegram", "provider", "interface_authoring", "worker"):
        _require_string(owners.get(key), f"telegram private_run.owners.{key}")
        _require_string(prefixes.get(key), f"telegram private_run.id_prefixes.{key}")
    for key in ("run", "ee", "ee_template", "worker_environment", "file_write_tool"):
        _require_string(prefixes.get(key), f"telegram private_run.id_prefixes.{key}")
    _require_int(
        private_run.get("authoring_max_attempts"),
        "telegram private_run.authoring_max_attempts",
        1,
        32,
    )
    _require_int(
        private_run.get("result_timeout_ms"),
        "telegram private_run.result_timeout_ms",
        1,
        600000,
    )


def resolve_base_url(config):
    bot_api = config["bot_api"]
    override = os.environ.get(bot_api["base_url_env"], "").strip()
    return override or bot_api["base_url_default"]


def resolve_token(config):
    token = os.environ.get(config["bot_api"]["token_env"], "").strip()
    if not token:
        raise RuntimeError(
            f"{config['bot_api']['token_env']} is empty; the bot token stays behind the "
            f"boundary and is named only by env var in config.json"
        )
    return token


def env_flag(name):
    if not name:
        return False
    return os.environ.get(name, "").strip().lower() in ("1", "true", "yes", "on")


def env_int_list(name):
    if not name:
        return []
    raw = os.environ.get(name, "").strip()
    if not raw:
        return []
    ids = []
    for part in raw.replace(",", " ").split():
        try:
            ids.append(int(part))
        except ValueError as error:
            raise ValueError(f"{name} contains non-integer id {part!r}") from error
    return ids


class TelegramError(RuntimeError):
    def __init__(self, method, body, source_error=None):
        self.method = method
        self.error_code = (body or {}).get("error_code")
        self.parameters = (body or {}).get("parameters") or {}
        self.description = (body or {}).get("description") or ""
        code = self.error_code if isinstance(self.error_code, int) and not isinstance(self.error_code, bool) else "unknown"
        source = source_error if isinstance(source_error, str) and re.fullmatch(r"[A-Za-z][A-Za-z0-9_]{0,127}", source_error) else None
        suffix = f" via {source}" if source is not None else ""
        super().__init__(f"telegram {method} failed with error_code {code}{suffix}")

    @property
    def retry_after(self):
        return self.parameters.get("retry_after")

    @property
    def is_flood(self):
        return self.error_code == 429 or "too many requests" in self.description.lower()

    @property
    def is_conflict(self):
        return self.error_code == 409 or "conflict" in self.description.lower()


class _SniHttpsConnection(http.client.HTTPSConnection):
    def __init__(self, ip, port, server_hostname, timeout_s, context):
        super().__init__(ip, port=port, timeout=timeout_s, context=context)
        self.server_hostname = server_hostname

    def connect(self):
        raw = socket.create_connection(
            (self.host, self.port),
            self.timeout,
            self.source_address,
        )
        if self._tunnel_host:
            self.sock = raw
            self._tunnel()
            raw = self.sock
        self.sock = self._context.wrap_socket(raw, server_hostname=self.server_hostname)


class _DirectHttpsResponse:
    def __init__(self, connection, response):
        self.connection = connection
        self.response = response
        self.headers = response.headers

    def read(self, amount=None):
        return self.response.read() if amount is None else self.response.read(amount)

    def close(self):
        try:
            self.response.close()
        finally:
            self.connection.close()

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def _read_socket_exact(sock, size):
    data = bytearray()
    while len(data) < size:
        chunk = sock.recv(size - len(data))
        if not chunk:
            raise OSError("proxy closed the connection during handshake")
        data.extend(chunk)
    return bytes(data)


def _proxy_credentials(parsed):
    username = urllib.parse.unquote(parsed.username) if parsed.username is not None else None
    password = urllib.parse.unquote(parsed.password) if parsed.password is not None else None
    if (username is None) != (password is None):
        raise ValueError("telegram proxy credentials require both username and password")
    return username, password


def sanitize_transport_error(value, bot_token=None, proxy_url=None):
    if isinstance(value, dict):
        return {key: sanitize_transport_error(item, bot_token, proxy_url) for key, item in value.items()}
    if isinstance(value, list):
        return [sanitize_transport_error(item, bot_token, proxy_url) for item in value]
    if isinstance(value, tuple):
        return tuple(sanitize_transport_error(item, bot_token, proxy_url) for item in value)
    if isinstance(value, BaseException):
        rendered = str(value)
        url = getattr(value, "url", None)
        if isinstance(url, str) and url and url not in rendered:
            rendered = f"{type(value).__name__}: {rendered}; url={url}"
        value = rendered
    if not isinstance(value, str):
        return value
    token = _ACTIVE_BOT_TOKEN if bot_token is None else bot_token
    configured_proxy = _ACTIVE_PROXY_URL if proxy_url is None else proxy_url
    sanitized = value
    if token:
        token_variants = {token}
        for item in tuple(token_variants):
            token_variants.add(urllib.parse.quote(item, safe=""))
            token_variants.add(urllib.parse.quote_plus(item, safe=""))
        for item in sorted(token_variants, key=len, reverse=True):
            sanitized = sanitized.replace(item, "[bot-token-redacted]")
    if not configured_proxy:
        return sanitized
    try:
        parsed = urllib.parse.urlparse(configured_proxy)
    except ValueError:
        return sanitized.replace(configured_proxy, "[proxy-redacted]")
    authority = parsed.netloc.rsplit("@", 1)[-1]
    redacted_proxy = (
        urllib.parse.urlunparse(
            (
                parsed.scheme,
                f"[credentials-redacted]@{authority}",
                parsed.path,
                parsed.params,
                parsed.query,
                parsed.fragment,
            )
        )
        if "@" in parsed.netloc
        else f"[proxy:{authority}]"
    )
    sentinel = "\x00octopus-telegram-proxy-url\x00"
    while sentinel in sanitized:
        sentinel += "_"
    sanitized = sanitized.replace(configured_proxy, sentinel)
    if "@" in parsed.netloc:
        encoded_userinfo = parsed.netloc.rsplit("@", 1)[0]
        try:
            username, password = _proxy_credentials(parsed)
        except (UnicodeDecodeError, ValueError):
            username, password = None, None
        decoded_userinfo = "" if username is None else f"{username}:{password}"
        tokens = {encoded_userinfo, decoded_userinfo}
        if decoded_userinfo:
            tokens.add(base64.b64encode(decoded_userinfo.encode("utf-8")).decode("ascii"))
        for item in (parsed.username or "", parsed.password or "", username or "", password or ""):
            if len(item) >= 3:
                tokens.add(item)
        for item in tuple(tokens):
            if item:
                tokens.add(urllib.parse.quote(item, safe=""))
                tokens.add(urllib.parse.quote_plus(item, safe=""))
        for item in sorted((item for item in tokens if item), key=len, reverse=True):
            sanitized = sanitized.replace(item, "[credentials-redacted]")
    return sanitized.replace(sentinel, redacted_proxy)


def _is_loopback_host(host):
    if not isinstance(host, str) or not host:
        return False
    lowered = host.lower().rstrip(".")
    if lowered == "localhost" or lowered.endswith(".localhost"):
        return True
    try:
        return ipaddress.ip_address(lowered).is_loopback
    except ValueError:
        return False


def _socks5_connect(parsed, target_host, target_port, timeout_s):
    proxy_port = parsed.port or 1080
    sock = socket.create_connection((parsed.hostname, proxy_port), timeout_s)
    sock.settimeout(timeout_s)
    username, password = _proxy_credentials(parsed)
    methods = b"\x00\x02" if username is not None else b"\x00"
    sock.sendall(b"\x05" + bytes([len(methods)]) + methods)
    version, method = _read_socket_exact(sock, 2)
    if version != 5 or method == 0xFF:
        sock.close()
        raise OSError("SOCKS5 proxy rejected authentication methods")
    if method == 2:
        if username is None:
            sock.close()
            raise OSError("SOCKS5 proxy requested credentials that were not configured")
        user = username.encode("utf-8")
        secret = password.encode("utf-8")
        if len(user) > 255 or len(secret) > 255:
            sock.close()
            raise ValueError("SOCKS5 proxy credentials exceed 255 bytes")
        sock.sendall(b"\x01" + bytes([len(user)]) + user + bytes([len(secret)]) + secret)
        auth_version, auth_status = _read_socket_exact(sock, 2)
        if auth_version != 1 or auth_status != 0:
            sock.close()
            raise OSError("SOCKS5 proxy authentication failed")
    elif method != 0:
        sock.close()
        raise OSError(f"SOCKS5 proxy selected unsupported auth method {method}")
    remote_dns = parsed.scheme == "socks5h"
    try:
        address = ipaddress.ip_address(target_host)
    except ValueError:
        address = None
    if address is not None and address.version == 4:
        encoded_host = b"\x01" + address.packed
    elif address is not None and address.version == 6:
        encoded_host = b"\x04" + address.packed
    elif remote_dns:
        encoded = target_host.encode("idna")
        if len(encoded) > 255:
            sock.close()
            raise ValueError("SOCKS5 target hostname exceeds 255 bytes")
        encoded_host = b"\x03" + bytes([len(encoded)]) + encoded
    else:
        resolved = socket.getaddrinfo(target_host, target_port, type=socket.SOCK_STREAM)
        if not resolved:
            sock.close()
            raise OSError(f"SOCKS5 could not resolve {target_host}")
        resolved_host = resolved[0][4][0]
        address = ipaddress.ip_address(resolved_host)
        encoded_host = (b"\x01" if address.version == 4 else b"\x04") + address.packed
    sock.sendall(b"\x05\x01\x00" + encoded_host + struct.pack("!H", target_port))
    version, status, reserved, address_type = _read_socket_exact(sock, 4)
    if version != 5 or reserved != 0 or status != 0:
        sock.close()
        raise OSError(f"SOCKS5 proxy connect failed with status {status}")
    if address_type == 1:
        _read_socket_exact(sock, 4)
    elif address_type == 4:
        _read_socket_exact(sock, 16)
    elif address_type == 3:
        length = _read_socket_exact(sock, 1)[0]
        _read_socket_exact(sock, length)
    else:
        sock.close()
        raise OSError("SOCKS5 proxy returned an invalid address type")
    _read_socket_exact(sock, 2)
    return sock


def _http_proxy_connect(parsed, target_host, target_port, timeout_s):
    proxy_port = parsed.port or (443 if parsed.scheme == "https" else 80)
    sock = socket.create_connection((parsed.hostname, proxy_port), timeout_s)
    sock.settimeout(timeout_s)
    if parsed.scheme == "https":
        context = ssl.create_default_context()
        sock = context.wrap_socket(sock, server_hostname=parsed.hostname)
    username, password = _proxy_credentials(parsed)
    host = target_host
    try:
        if ipaddress.ip_address(target_host).version == 6:
            host = f"[{target_host}]"
    except ValueError:
        pass
    authority = f"{host}:{target_port}"
    headers = [
        f"CONNECT {authority} HTTP/1.1",
        f"Host: {authority}",
        "Proxy-Connection: Keep-Alive",
    ]
    if username is not None:
        credentials = base64.b64encode(f"{username}:{password}".encode("utf-8")).decode("ascii")
        headers.append(f"Proxy-Authorization: Basic {credentials}")
    sock.sendall(("\r\n".join(headers) + "\r\n\r\n").encode("ascii"))
    response = bytearray()
    while b"\r\n\r\n" not in response:
        chunk = sock.recv(4096)
        if not chunk:
            sock.close()
            raise OSError("HTTP proxy closed before CONNECT response")
        response.extend(chunk)
        if len(response) > 65536:
            sock.close()
            raise OSError("HTTP proxy CONNECT response exceeded 64 KiB")
    status_line = bytes(response).split(b"\r\n", 1)[0]
    pieces = status_line.split(b" ", 2)
    if len(pieces) < 2 or not pieces[1].isdigit() or int(pieces[1]) != 200:
        sock.close()
        status = int(pieces[1]) if len(pieces) >= 2 and pieces[1].isdigit() else None
        suffix = f" with status {status}" if status is not None else " with malformed status"
        raise OSError(f"HTTP proxy CONNECT failed{suffix}")
    return sock


def _proxy_tunnel(parsed, target_host, target_port, timeout_s):
    if parsed.scheme in ("socks5", "socks5h"):
        return _socks5_connect(parsed, target_host, target_port, timeout_s)
    if parsed.scheme in ("http", "https"):
        return _http_proxy_connect(parsed, target_host, target_port, timeout_s)
    raise ValueError(f"telegram proxy scheme {parsed.scheme!r} is unsupported")


class BotApi:
    def __init__(self, base_url, token, config):
        global _ACTIVE_BOT_TOKEN, _ACTIVE_PROXY_URL
        self.base = base_url.rstrip("/")
        parsed = urllib.parse.urlparse(self.base)
        if (
            parsed.scheme not in ("http", "https")
            or not parsed.hostname
            or parsed.username is not None
            or parsed.password is not None
            or parsed.query
            or parsed.fragment
        ):
            raise ValueError("telegram Bot API base URL must be an absolute HTTP(S) origin without credentials")
        if parsed.scheme == "http" and not _is_loopback_host(parsed.hostname):
            raise ValueError("telegram Bot API base URL must use HTTPS outside loopback")
        self.token = token
        self.endpoint = f"{self.base}/bot{token}"
        self.file_endpoint = f"{self.base}/file/bot{token}"
        self.timeout_s = max(1.0, config["bot_api"]["request_timeout_ms"] / 1000.0)
        self.max_response_bytes = config["bot_api"]["max_response_bytes"]
        self.max_multipart_bytes = config["media"]["max_multipart_bytes"]
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), _NoRedirect())
        self.official_origin = parsed.scheme == "https" and parsed.hostname == "api.telegram.org"
        proxy_raw = os.environ.get(config["bot_api"]["proxy_env"], "").strip()
        self.proxy_raw = proxy_raw
        self.proxy = urllib.parse.urlparse(proxy_raw) if proxy_raw else None
        if self.proxy is not None:
            if (
                self.proxy.scheme not in ("http", "https", "socks5", "socks5h")
                or not self.proxy.hostname
                or self.proxy.query
                or self.proxy.fragment
                or self.proxy.path not in ("", "/")
            ):
                raise ValueError(
                    "telegram proxy must be an HTTP(S) or SOCKS5 URL without path, query, or fragment"
                )
            proxy_username, _ = _proxy_credentials(self.proxy)
            if proxy_username is not None and self.proxy.scheme != "https" and not _is_loopback_host(self.proxy.hostname):
                raise ValueError("telegram authenticated proxy transport must use HTTPS or loopback")
        _ACTIVE_BOT_TOKEN = token
        _ACTIVE_PROXY_URL = proxy_raw
        self.local_mode = env_flag(config["bot_api"]["local_mode_env"])
        try:
            slots = json.loads(os.environ.get("OCTOPUS_ROOT_SLOTS_JSON", "{}"))
        except json.JSONDecodeError as error:
            raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON is malformed") from error
        if not isinstance(slots, dict):
            raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON must be an object")
        self.local_file_roots = []
        for slot in config["bot_api"]["local_file_root_slots"]:
            path = slots.get(slot)
            if path is not None:
                if not isinstance(path, str) or not os.path.isabs(path):
                    raise RuntimeError(f"telegram local Bot API root slot {slot!r} is not absolute")
                self.local_file_roots.append(os.path.realpath(path))
        raw_local_roots = os.environ.get(config["bot_api"]["local_file_roots_env"], "")
        for raw_root in raw_local_roots.split(os.pathsep):
            if not raw_root:
                continue
            if not os.path.isabs(raw_root):
                raise ValueError("telegram local Bot API file roots must be absolute")
            resolved_root = os.path.realpath(raw_root)
            if resolved_root not in self.local_file_roots:
                self.local_file_roots.append(resolved_root)
        if self.local_mode and not self.local_file_roots:
            raise RuntimeError(
                "telegram local Bot API mode requires at least one resolved local file root"
            )
        self.fallback_ips = []
        self.sticky_ip = None
        self.fallback_ips = self._resolve_fallback_ips(config)

    def sanitize_error(self, value):
        return sanitize_transport_error(value, self.token, self.proxy_raw)

    def _resolve_fallback_ips(self, config):
        bot_api = config["bot_api"]
        raw = os.environ.get(bot_api.get("fallback_ips_env", ""), "").strip()
        ips = [p.strip() for p in raw.replace(",", " ").split() if p.strip()]
        if not ips:
            ips = self._discover_fallback_ips(bot_api)
            ips.extend(bot_api.get("seed_fallback_ips", []))
        valid = []
        for ip in ips:
            try:
                normalized = str(ipaddress.ip_address(ip))
            except ValueError as error:
                raise ValueError(f"telegram fallback IP {ip!r} is invalid") from error
            if normalized not in valid:
                valid.append(normalized)
        return valid

    def _discover_fallback_ips(self, bot_api):
        discovered = []
        for provider in bot_api.get("fallback_discovery", []):
            request = urllib.request.Request(provider["url"], headers=provider.get("headers", {}))
            try:
                with self._open(request, min(self.timeout_s, 10.0)) as response:
                    raw = response.read(min(self.max_response_bytes, 1048576) + 1)
                if len(raw) > min(self.max_response_bytes, 1048576):
                    continue
                value = json.loads(raw.decode("utf-8"))
                for key in provider["answers_path"]:
                    if not isinstance(value, dict) or key not in value:
                        value = None
                        break
                    value = value[key]
                if not isinstance(value, list):
                    continue
                for answer in value:
                    if not isinstance(answer, dict):
                        continue
                    if answer.get(provider["record_type_key"]) != provider["record_type_value"]:
                        continue
                    address = answer.get(provider["address_key"])
                    try:
                        normalized = str(ipaddress.ip_address(address))
                    except (TypeError, ValueError):
                        continue
                    if normalized not in discovered:
                        discovered.append(normalized)
            except (OSError, ValueError, urllib.error.URLError):
                continue
        return discovered

    def _open(self, request, timeout_s):
        host = urllib.parse.urlparse(request.full_url).hostname
        attempts = [None]
        if self.official_origin:
            if self.sticky_ip:
                attempts = [self.sticky_ip, None]
            attempts += [ip for ip in self.fallback_ips if ip not in attempts]
        last_error = None
        for ip in attempts:
            try:
                if self.proxy is not None:
                    opened = self._open_via_proxy(request, ip, host, timeout_s)
                elif ip is None:
                    opened = self.opener.open(request, timeout=timeout_s)
                else:
                    opened = self._open_via_ip(request, ip, host, timeout_s)
                if ip is not None:
                    self.sticky_ip = ip
                return opened
            except urllib.error.HTTPError:
                raise
            except (urllib.error.URLError, OSError) as error:
                last_error = error
                if ip is not None and ip == self.sticky_ip:
                    self.sticky_ip = None
                continue
        if last_error is not None:
            raise urllib.error.URLError(self.sanitize_error(last_error)) from None
        raise urllib.error.URLError("no endpoint reachable")

    def _open_via_ip(self, request, ip, host, timeout_s):
        parsed = urllib.parse.urlparse(request.full_url)
        if parsed.scheme != "https" or not host:
            raise urllib.error.URLError("direct-IP fallback requires an HTTPS URL with a hostname")
        port = parsed.port or 443
        context = ssl.create_default_context()
        connection = _SniHttpsConnection(ip, port, host, timeout_s, context)
        path = urllib.parse.urlunparse(("", "", parsed.path or "/", parsed.params, parsed.query, ""))
        headers = dict(request.header_items())
        headers["Host"] = host if port == 443 else f"{host}:{port}"
        try:
            connection.request(request.get_method(), path, body=request.data, headers=headers)
            response = connection.getresponse()
        except Exception:
            connection.close()
            raise
        wrapped = _DirectHttpsResponse(connection, response)
        if response.status >= 400:
            raise urllib.error.HTTPError(
                request.full_url,
                response.status,
                response.reason,
                response.headers,
                wrapped,
            )
        return wrapped

    def _open_via_proxy(self, request, ip, host, timeout_s):
        parsed = urllib.parse.urlparse(request.full_url)
        if parsed.scheme != "https" or not host:
            raise urllib.error.URLError("telegram proxy transport requires an HTTPS target")
        port = parsed.port or 443
        connect_host = ip or host
        raw_socket = _proxy_tunnel(self.proxy, connect_host, port, timeout_s)
        context = ssl.create_default_context()
        try:
            tls_socket = context.wrap_socket(raw_socket, server_hostname=host)
            connection = http.client.HTTPSConnection(host, port, timeout=timeout_s, context=context)
            connection.sock = tls_socket
            path = urllib.parse.urlunparse(
                ("", "", parsed.path or "/", parsed.params, parsed.query, "")
            )
            headers = dict(request.header_items())
            headers["Host"] = host if port == 443 else f"{host}:{port}"
            connection.request(request.get_method(), path, body=request.data, headers=headers)
            response = connection.getresponse()
        except Exception:
            raw_socket.close()
            raise
        wrapped = _DirectHttpsResponse(connection, response)
        if response.status >= 400:
            raise urllib.error.HTTPError(
                request.full_url,
                response.status,
                response.reason,
                response.headers,
                wrapped,
            )
        return wrapped

    def call(self, method, params=None, timeout_s=None):
        data = json.dumps(params or {}).encode("utf-8")
        request = urllib.request.Request(
            f"{self.endpoint}/{method}",
            data=data,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with self._open(request, timeout_s or self.timeout_s) as response:
                raw = response.read(self.max_response_bytes + 1)
                if len(raw) > self.max_response_bytes:
                    raise RuntimeError(f"telegram {method} response exceeds max_response_bytes")
                body = json.loads(raw.decode("utf-8"))
        except urllib.error.HTTPError as error:
            try:
                raw = error.read(self.max_response_bytes + 1)
                body = json.loads(raw.decode("utf-8")) if len(raw) <= self.max_response_bytes else {}
            except (ValueError, OSError):
                body = {}
            if not isinstance(body, dict):
                body = {}
            body.setdefault("ok", False)
            body.setdefault("error_code", error.code)
            body.setdefault("description", self.sanitize_error(error))
            raise TelegramError(method, self.sanitize_error(body), type(error).__name__) from None
        except (urllib.error.URLError, OSError) as error:
            raise urllib.error.URLError(self.sanitize_error(error)) from None
        if not isinstance(body, dict):
            raise RuntimeError(f"telegram {method} response is not a JSON object")
        if not body.get("ok", False):
            raise TelegramError(method, self.sanitize_error(body))
        return body.get("result")

    def call_multipart(self, method, params, files, timeout_s=None):
        total_file_bytes = sum(len(file_spec[2]) for file_spec in files.values())
        if total_file_bytes > self.max_multipart_bytes:
            raise ValueError("telegram multipart upload exceeds max_multipart_bytes")
        boundary = f"octopus-{secrets.token_hex(24)}"
        body = bytearray()
        for name, value in params.items():
            if value is None:
                continue
            if isinstance(value, (dict, list)):
                encoded = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
            elif isinstance(value, bool):
                encoded = "true" if value else "false"
            elif isinstance(value, (str, int, float)) and not isinstance(value, bool):
                encoded = str(value)
            else:
                raise ValueError(f"telegram multipart field {name!r} has unsupported type")
            body.extend(f"--{boundary}\r\n".encode("ascii"))
            body.extend(
                f'Content-Disposition: form-data; name="{name}"\r\n\r\n'.encode("ascii")
            )
            body.extend(encoded.encode("utf-8"))
            body.extend(b"\r\n")
        for field, file_spec in files.items():
            filename, content_type, payload = file_spec
            if not isinstance(payload, bytes):
                raise ValueError(f"telegram multipart file {field!r} is not bytes")
            safe_filename = re.sub(r"[^A-Za-z0-9._-]", "_", filename) or "upload.bin"
            body.extend(f"--{boundary}\r\n".encode("ascii"))
            body.extend(
                f'Content-Disposition: form-data; name="{field}"; filename="{safe_filename}"\r\n'.encode(
                    "ascii"
                )
            )
            body.extend(f"Content-Type: {content_type}\r\n\r\n".encode("ascii"))
            body.extend(payload)
            body.extend(b"\r\n")
        body.extend(f"--{boundary}--\r\n".encode("ascii"))
        request = urllib.request.Request(
            f"{self.endpoint}/{method}",
            data=bytes(body),
            headers={"Content-Type": f"multipart/form-data; boundary={boundary}"},
            method="POST",
        )
        try:
            with self._open(request, timeout_s or self.timeout_s) as response:
                raw = response.read(self.max_response_bytes + 1)
                if len(raw) > self.max_response_bytes:
                    raise RuntimeError(f"telegram {method} response exceeds max_response_bytes")
                decoded = json.loads(raw.decode("utf-8"))
        except urllib.error.HTTPError as error:
            try:
                raw = error.read(self.max_response_bytes + 1)
                decoded = json.loads(raw.decode("utf-8")) if len(raw) <= self.max_response_bytes else {}
            except (ValueError, OSError):
                decoded = {}
            if not isinstance(decoded, dict):
                decoded = {}
            decoded.setdefault("ok", False)
            decoded.setdefault("error_code", error.code)
            decoded.setdefault("description", self.sanitize_error(error))
            raise TelegramError(method, self.sanitize_error(decoded), type(error).__name__) from None
        except (urllib.error.URLError, OSError) as error:
            raise urllib.error.URLError(self.sanitize_error(error)) from None
        if not isinstance(decoded, dict):
            raise RuntimeError(f"telegram {method} response is not a JSON object")
        if not decoded.get("ok", False):
            raise TelegramError(method, self.sanitize_error(decoded))
        return decoded.get("result")

    def call_flood_safe(self, method, params, config):
        outbound = config["outbound"]
        max_retries = outbound.get("flood_max_retries", 3)
        for _attempt in range(max_retries):
            try:
                return self.call(method, params)
            except TelegramError as error:
                if not error.is_flood:
                    raise
                time.sleep(self._flood_wait_seconds(error, outbound))
        return self.call(method, params)

    def call_multipart_flood_safe(self, method, params, files, config):
        outbound = config["outbound"]
        max_retries = outbound["flood_max_retries"]
        for _attempt in range(max_retries):
            try:
                return self.call_multipart(method, params, files)
            except TelegramError as error:
                if not error.is_flood:
                    raise
                time.sleep(self._flood_wait_seconds(error, outbound))
        return self.call_multipart(method, params, files)

    def _flood_wait_seconds(self, error, outbound):
        value = error.retry_after
        if value is None:
            wait = outbound["flood_backoff_ms"] / 1000.0
        elif isinstance(value, bool) or not isinstance(value, (int, float)):
            raise RuntimeError("telegram flood retry_after is not numeric")
        else:
            wait = float(value)
        maximum = outbound["flood_max_wait_ms"] / 1000.0
        if not math.isfinite(wait) or wait < 0 or wait > maximum:
            raise RuntimeError("telegram flood retry_after exceeds flood_max_wait_ms")
        return wait

    def download_file(self, file_path, max_bytes):
        if os.path.isabs(file_path):
            if not self.local_mode:
                raise ValueError("telegram returned an absolute file path outside local Bot API mode")
            failures = []
            for root in self.local_file_roots:
                try:
                    return _read_rooted_regular(root, file_path, max_bytes)
                except TelegramMediaTooLarge:
                    raise
                except (FileNotFoundError, NotADirectoryError, PermissionError, ValueError, OSError) as error:
                    failures.append(error)
            if failures:
                raise ValueError("telegram local Bot API file path is outside or unsafe for configured roots") from failures[-1]
            raise ValueError("telegram local Bot API file path has no configured root")
        parsed = urllib.parse.urlparse(file_path)
        if (
            parsed.scheme
            or parsed.netloc
            or parsed.query
            or parsed.fragment
            or file_path.startswith("/")
            or any(part == ".." for part in file_path.split("/"))
        ):
            raise ValueError("telegram file_path is not a safe relative Bot API path")
        request = urllib.request.Request(
            f"{self.file_endpoint}/{urllib.parse.quote(file_path, safe='/')}"
        )
        try:
            with self._open(request, self.timeout_s) as response:
                payload = response.read(max_bytes + 1)
            if len(payload) > max_bytes:
                raise TelegramMediaTooLarge(max_bytes)
            return payload
        except (urllib.error.HTTPError, urllib.error.URLError, OSError) as error:
            raise urllib.error.URLError(self.sanitize_error(error)) from None


def load_intake_offset():
    try:
        with _open_private_read(INTAKE_CURSOR_PATH) as handle:
            state = json.loads(handle.read().decode("utf-8"))
    except FileNotFoundError:
        return 0
    state = _require_keys(state, {"offset"}, set(), "telegram intake cursor")
    return _require_int(state["offset"], "telegram intake cursor.offset", 0)


def commit_intake_offset(offset):
    _atomic_write_json(INTAKE_CURSOR_PATH, {"offset": offset})


def _atomic_write_json(path, obj, indent=None):
    payload = json.dumps(obj, indent=indent, ensure_ascii=False).encode("utf-8")
    _atomic_write_bytes(path, payload)


def _atomic_write_bytes(path, payload):
    parent = os.path.dirname(path)
    _ensure_private_directory(parent)
    tmp = os.path.join(parent, f".{os.path.basename(path)}.{os.getpid()}.{time.time_ns()}.tmp")
    try:
        descriptor = _open_private_file(tmp, os.O_CREAT | os.O_EXCL | os.O_WRONLY, create=True)
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        try:
            existing = os.lstat(path)
            if stat.S_ISLNK(existing.st_mode) or not stat.S_ISREG(existing.st_mode):
                raise PermissionError(f"private path is not a regular file: {path}")
            _require_private_mode(existing, 0o600, path)
        except FileNotFoundError:
            pass
        os.replace(tmp, path)
        directory = os.open(parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass


def _read_runtime_verdicts():
    global _VERDICT_INPUT_ERROR
    try:
        for line in sys.stdin:
            message = json.loads(line)
            if message.get("kind") != "delivery_verdict":
                raise RuntimeError(f"unexpected runtime input: {message!r}")
            request_id = message.get("request_id")
            if not isinstance(request_id, int):
                raise RuntimeError(f"runtime verdict has no integer request_id: {message!r}")
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


def hand_artifact(manifest_path):
    global _NEXT_REQUEST_ID
    with _VERDICT_CONDITION:
        request_id = _NEXT_REQUEST_ID
        _NEXT_REQUEST_ID += 1
    with _STDOUT_LOCK:
        sys.stdout.write(
            json.dumps(
                {
                    "kind": "arrival",
                    "request_id": request_id,
                    "artifact_manifest_path": manifest_path,
                    "evidence": [],
                }
            )
            + "\n"
        )
        sys.stdout.flush()
    with _VERDICT_CONDITION:
        while request_id not in _VERDICTS and _VERDICT_INPUT_ERROR is None:
            _VERDICT_CONDITION.wait()
        if _VERDICT_INPUT_ERROR is not None:
            raise RuntimeError(
                f"runtime verdict input failed before request {request_id} admission: "
                f"{_VERDICT_INPUT_ERROR}"
            )
        verdict = _VERDICTS.pop(request_id)
    admitted = verdict.get("admitted")
    if isinstance(admitted, dict) and isinstance(admitted.get("seq"), int):
        return admitted["seq"]
    raise RuntimeError(f"runtime refused telegram artifact: {verdict!r}")


def materialize_runtime_artifact(label, payload, key):
    payload_path = os.path.join(PAYLOADS_DIR, f"{key}.payload")
    manifest_path = os.path.join(PAYLOADS_DIR, f"{key}.artifact.json")
    _atomic_write_bytes(payload_path, payload)
    _atomic_write_json(manifest_path, {"label": label, "payload_path": payload_path})
    return manifest_path


def hand_failure(source_seq, failure_class, details, key):
    payload = json.dumps(
        {
            "source_movement_seq": source_seq,
            "failure_class": failure_class,
            "details": sanitize_transport_error(details),
        },
        sort_keys=True,
    ).encode("utf-8")
    return hand_artifact(
        materialize_runtime_artifact(f"{INTERFACE_ID}.failure", payload, key)
    )


def _root_slots():
    try:
        slots = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
    except (KeyError, json.JSONDecodeError) as error:
        raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON is absent or malformed") from error
    if not isinstance(slots, dict):
        raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON must be an object")
    resolved = {}
    for name, path in slots.items():
        if not isinstance(name, str) or not name:
            raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON contains an invalid slot name")
        if not isinstance(path, str) or not os.path.isabs(path):
            raise RuntimeError(f"OCTOPUS_ROOT_SLOTS_JSON has non-absolute slot {name!r}")
        resolved[name] = os.path.realpath(path)
    for name in (
        "registry_interface_root",
        "registry_ee_template_root",
        "registry_tool_root",
        "registry_worker_environment_kind_root",
        "run_records_root",
        "ee_instances_root",
    ):
        if name not in resolved:
            raise RuntimeError(f"OCTOPUS_ROOT_SLOTS_JSON has no absolute {name!r}")
    return resolved


def _octopus_home():
    home = os.path.dirname(os.path.dirname(os.path.realpath(VAR_DIR)))
    if not os.path.isabs(home):
        raise RuntimeError("telegram interface var directory does not resolve an Octopus home")
    return home


def _json_text(value):
    return json.dumps(value, indent=2, ensure_ascii=False, sort_keys=False) + "\n"


def _identity_salt():
    try:
        with _open_private_read(IDENTITY_SALT_PATH) as handle:
            salt = handle.read()
    except FileNotFoundError:
        salt = secrets.token_bytes(32)
        _atomic_write_bytes(IDENTITY_SALT_PATH, salt)
    if len(salt) != 32:
        raise ValueError("telegram identity salt must contain exactly 32 bytes")
    return salt


def _binding_secret_path(binding):
    opaque_id = _require_string(binding.get("opaque_id"), "telegram binding opaque_id")
    if not re.fullmatch(r"[0-9a-f]{24}", opaque_id):
        raise ValueError("telegram binding opaque_id is malformed")
    root = os.path.realpath(BINDING_SECRETS_DIR)
    path = os.path.abspath(os.path.join(root, f"{opaque_id}.key"))
    if os.path.commonpath((path, root)) != root:
        raise ValueError("telegram binding secret handle escaped its private directory")
    return path


def _load_secret_handle(path):
    raw_path = _require_string(path, "telegram binding secret_handle")
    if not os.path.isabs(raw_path):
        raise RuntimeError("telegram binding secret_handle is unavailable")
    path = os.path.abspath(raw_path)
    with _open_private_read(path) as handle:
        secret = handle.read()
    if len(secret) != 32:
        raise RuntimeError("telegram binding secret_handle is malformed")
    return secret


def _ensure_binding_secret(binding):
    path = _binding_secret_path(binding)
    try:
        return path, _load_secret_handle(path)
    except FileNotFoundError:
        secret = secrets.token_bytes(32)
        _atomic_write_bytes(path, secret)
        return path, secret


def _binding_secret(binding):
    return _load_secret_handle(_binding_secret_path(binding))


def _binding_coordinate_key(chat_id, thread_id):
    validate_chat_coordinates(chat_id, thread_id)
    return f"{chat_id}:{'' if thread_id is None else thread_id}"


def _load_bindings():
    try:
        state = json.loads(
            _read_bounded_regular(
                BINDINGS_PATH,
                TOPOLOGY_MAX_BYTES,
                "binding state",
            )
        )
    except FileNotFoundError:
        return {"schema_version": 1, "bindings": {}}
    if not isinstance(state, dict) or set(state) != {"schema_version", "bindings"}:
        raise ValueError("telegram binding state must contain schema_version and bindings")
    if state["schema_version"] != 1 or not isinstance(state["bindings"], dict):
        raise ValueError("telegram binding state has an unsupported shape")
    identities = {field: set() for field in ("opaque_id", "interface", "worker_environment_interface", "file_write_tool", "run", "ee")}
    for key, binding in state["bindings"].items():
        _require_string(key, "telegram binding state key")
        _validate_binding_record(binding, key)
        for field, values in identities.items():
            value = binding[field]
            if value in values:
                raise ValueError(f"telegram binding state repeats {field} {value!r}")
            values.add(value)
    return state


def _validate_binding_record(binding, key):
    required = {
        "chat_id",
        "thread_id",
        "chat_type",
        "opaque_id",
        "interface",
        "provider_interface",
        "interface_authoring_interface",
        "worker_environment_interface",
        "file_write_tool",
        "worker_interface",
        "run",
        "ee",
        "host",
        "port",
        "status",
    }
    _require_keys(binding, required, set(), f"telegram binding state {key!r}")
    validate_chat_coordinates(binding["chat_id"], binding["thread_id"])
    if _binding_coordinate_key(binding["chat_id"], binding["thread_id"]) != key:
        raise ValueError(f"telegram binding state {key!r} has mismatched coordinates")
    _require_string(binding["opaque_id"], f"telegram binding state {key!r}.opaque_id")
    for field in (
        "interface",
        "provider_interface",
        "interface_authoring_interface",
        "worker_environment_interface",
        "file_write_tool",
        "worker_interface",
        "run",
        "ee",
        "host",
    ):
        _require_string(binding[field], f"telegram binding state {key!r}.{field}")
    if not _is_loopback_host(binding["host"]):
        raise ValueError(f"telegram binding state {key!r}.host must be loopback")
    _require_int(binding["port"], f"telegram binding state {key!r}.port", 1, 65535)
    if binding["status"] not in ("planned", "ready"):
        raise ValueError(f"telegram binding state {key!r}.status is invalid")


def _binding_by_interface(interface):
    with _STATE_LOCK:
        matches = [
            binding
            for binding in _load_bindings()["bindings"].values()
            if binding["interface"] == interface
        ]
    if len(matches) != 1:
        return None
    return matches[0]


def _ensure_binding_record(config, chat_id, thread_id, host, port):
    host = _require_string(host, "telegram binding host")
    if not _is_loopback_host(host):
        raise ValueError("telegram binding host must be loopback")
    port = _require_int(port, "telegram binding port", 1, 65535)
    key = _binding_coordinate_key(chat_id, thread_id)
    with _STATE_LOCK:
        state = _load_bindings()
        existing = state["bindings"].get(key)
        if existing is not None:
            existing["host"] = host
            existing["port"] = port
            _ensure_binding_secret(existing)
            _atomic_write_json(BINDINGS_PATH, state, indent=2)
            return existing
        opaque = hashlib.sha256(
            _identity_salt() + b"\0" + key.encode("utf-8")
        ).hexdigest()[:24]
        prefixes = config["private_run"]["id_prefixes"]
        binding = {
            "chat_id": chat_id,
            "thread_id": thread_id,
            "chat_type": None,
            "opaque_id": opaque,
            "interface": f"{prefixes['telegram']}{opaque}",
            "provider_interface": f"{prefixes['provider']}{opaque}",
            "interface_authoring_interface": f"{prefixes['interface_authoring']}{opaque}",
            "worker_environment_interface": f"{prefixes['worker_environment']}{opaque}",
            "file_write_tool": f"{prefixes['file_write_tool']}{opaque}",
            "worker_interface": f"{prefixes['worker']}{opaque}",
            "run": f"{prefixes['run']}{opaque}",
            "ee": f"{prefixes['ee']}{opaque}",
            "host": host,
            "port": port,
            "status": "planned",
        }
        _ensure_binding_secret(binding)
        _validate_binding_record(binding, key)
        state["bindings"][key] = binding
        _atomic_write_json(BINDINGS_PATH, state, indent=2)
        return binding


def _mark_binding_ready(binding):
    key = _binding_coordinate_key(binding["chat_id"], binding["thread_id"])
    with _STATE_LOCK:
        state = _load_bindings()
        current = state["bindings"].get(key)
        planned = {**binding, "status": "planned"}
        ready = {**binding, "status": "ready"}
        if current not in (planned, ready):
            raise RuntimeError(f"telegram binding {key!r} changed while it was lifting")
        state["bindings"][key] = ready
        _atomic_write_json(BINDINGS_PATH, state, indent=2)
        binding["status"] = "ready"


def _read_frame(sock, buffer, max_bytes):
    while True:
        newline = buffer.find(b"\n")
        if newline >= 0:
            frame = bytes(buffer[:newline])
            del buffer[: newline + 1]
            if not frame:
                raise RuntimeError("telegram driver IPC received an empty frame")
            try:
                value = json.loads(frame)
            except json.JSONDecodeError as error:
                raise RuntimeError(f"telegram driver IPC frame is malformed: {error}") from error
            if not isinstance(value, dict):
                raise RuntimeError("telegram driver IPC frame must be an object")
            return value
        if len(buffer) >= max_bytes:
            raise RuntimeError("telegram driver IPC frame exceeds max_frame_bytes")
        chunk = sock.recv(min(65536, max_bytes - len(buffer)))
        if not chunk:
            raise ConnectionError("telegram driver IPC peer closed")
        buffer.extend(chunk)


def _write_frame(sock, value, max_bytes):
    payload = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"
    if len(payload) > max_bytes:
        raise RuntimeError("telegram driver IPC frame exceeds max_frame_bytes")
    sock.sendall(payload)


class _BindingConnection:
    def __init__(self, sock, interface, max_frame_bytes, timeout_s):
        self.sock = sock
        self.interface = interface
        self.max_frame_bytes = max_frame_bytes
        self.timeout_s = timeout_s
        self.buffer = bytearray()
        self.lock = threading.Lock()
        self.closed = threading.Event()
        self.next_request_id = 1

    def request(self, update):
        with self.lock:
            request_id = self.next_request_id
            self.next_request_id += 1
            self.sock.settimeout(self.timeout_s)
            try:
                _write_frame(
                    self.sock,
                    {"kind": "update", "request_id": request_id, "update": update},
                    self.max_frame_bytes,
                )
                reply = _read_frame(self.sock, self.buffer, self.max_frame_bytes)
            except Exception:
                self.close()
                raise
            if reply.get("kind") != "update_verdict" or reply.get("request_id") != request_id:
                self.close()
                raise RuntimeError(
                    f"telegram binding {self.interface!r} returned an invalid delivery verdict"
                )
            if reply.get("ok") is not True or not isinstance(reply.get("admitted_seq"), int):
                raise RuntimeError(
                    f"telegram binding {self.interface!r} refused update: {reply.get('error')!r}"
                )
            return reply["admitted_seq"]

    def close(self):
        if self.closed.is_set():
            return
        self.closed.set()
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.sock.close()


class BindingHub:
    def __init__(self, config):
        self.config = config
        self.condition = threading.Condition()
        self.connections = {}
        self.max_frame_bytes = config["driver_ipc"]["max_frame_bytes"]
        self.request_timeout_s = config["driver_ipc"]["request_timeout_ms"] / 1000.0
        self.registration_slots = threading.BoundedSemaphore(PRIVATE_REGISTRATION_LIMIT)
        self.listener, self.host, self.port = self._bind_listener()
        threading.Thread(target=self._accept_loop, daemon=True).start()

    def _bind_listener(self):
        ipc = self.config["driver_ipc"]
        state_path = os.path.join(VAR_DIR, ipc["state_file"])
        configured_host = ipc["listen"]
        configured_port = ipc["port"]
        try:
            with open(state_path, "r", encoding="utf-8") as handle:
                state = json.load(handle)
        except FileNotFoundError:
            state = None
        if state is not None:
            if not isinstance(state, dict) or set(state) != {"host", "port"}:
                raise ValueError("telegram driver IPC state is malformed")
            host = _require_string(state["host"], "telegram driver IPC state.host")
            port = _require_int(state["port"], "telegram driver IPC state.port", 1, 65535)
            if host != configured_host or configured_port not in (0, port):
                raise RuntimeError("telegram driver IPC config disagrees with its durable address")
        else:
            host, port = configured_host, configured_port
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            listener.bind((host, port))
            listener.listen(128)
        except Exception:
            listener.close()
            raise
        actual_host, actual_port = listener.getsockname()
        if state is None:
            _atomic_write_json(state_path, {"host": actual_host, "port": actual_port}, indent=2)
        return listener, actual_host, actual_port

    def _accept_loop(self):
        while True:
            sock, _address = self.listener.accept()
            if not self.registration_slots.acquire(blocking=False):
                sock.close()
                continue
            try:
                threading.Thread(target=self._register_socket, args=(sock,), daemon=True).start()
            except Exception:
                self.registration_slots.release()
                sock.close()
                raise

    def _register_socket(self, sock):
        buffer = bytearray()
        sock.settimeout(self.config["driver_ipc"]["connect_timeout_ms"] / 1000.0)
        connection = None
        registration_released = False
        try:
            registration_maximum = min(self.max_frame_bytes, PRIVATE_REGISTRATION_MAX_BYTES)
            frame = _read_frame(sock, buffer, registration_maximum)
            if frame.get("kind") != "register":
                raise RuntimeError("telegram driver IPC first frame must be register")
            interface = _require_string(frame.get("interface"), "telegram driver IPC interface")
            secret = _require_string(frame.get("secret"), "telegram driver IPC secret")
            binding = _binding_by_interface(interface)
            if binding is None or not hmac.compare_digest(_binding_secret(binding).hex(), secret):
                raise RuntimeError("telegram driver IPC registration was not authorized")
            connection = _BindingConnection(
                sock,
                interface,
                self.max_frame_bytes,
                self.request_timeout_s,
            )
            connection.buffer.extend(buffer)
            _write_frame(sock, {"kind": "registered", "interface": interface}, registration_maximum)
            with self.condition:
                previous = self.connections.get(interface)
                self.connections[interface] = connection
                self.condition.notify_all()
            if previous is not None:
                previous.close()
            self.registration_slots.release()
            registration_released = True
            connection.closed.wait()
        except Exception as error:
            sys.stderr.write(f"telegram driver IPC registration failed: {sanitize_transport_error(error)}\n")
            sys.stderr.flush()
        finally:
            if not registration_released:
                self.registration_slots.release()
            if connection is not None:
                with self.condition:
                    if self.connections.get(connection.interface) is connection:
                        self.connections.pop(connection.interface, None)
                connection.close()
            else:
                sock.close()

    def wait_for(self, interface, timeout_ms):
        deadline = time.monotonic() + timeout_ms / 1000.0
        with self.condition:
            while True:
                connection = self.connections.get(interface)
                if connection is not None and not connection.closed.is_set():
                    return connection
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(
                        f"telegram binding interface {interface!r} did not lift within {timeout_ms}ms"
                    )
                self.condition.wait(remaining)

    def dispatch(self, binding, update):
        timeout_ms = self.config["driver_ipc"]["connect_timeout_ms"]
        connection = self.wait_for(binding["interface"], timeout_ms)
        try:
            return connection.request(update)
        except Exception:
            with self.condition:
                if self.connections.get(binding["interface"]) is connection:
                    self.connections.pop(binding["interface"], None)
            raise


class IntakeBatcher:
    def __init__(self, config, hub):
        self.config = config
        self.hub = hub
        self.settings = config["intake"]["batching"]
        self.lock = threading.RLock()
        self.condition = threading.Condition(self.lock)
        self._recover_claims()
        threading.Thread(target=self._run, daemon=True).start()

    def _merge_states(self, earlier, later):
        if earlier["interface"] != later["interface"]:
            raise ValueError("telegram pending intake merge crosses interfaces")
        by_id = {item["update_id"]: item for item in earlier["updates"]}
        by_id.update({item["update_id"]: item for item in later["updates"]})
        merged = dict(later)
        merged["updates"] = sorted(by_id.values(), key=lambda item: item["update_id"])
        merged["attempt"] = max(earlier["attempt"], later["attempt"])
        merged["deadline_unix_ms"] = max(
            earlier["deadline_unix_ms"], later["deadline_unix_ms"]
        )
        return merged

    def _recover_claims(self):
        for name in os.listdir(PENDING_INTAKE_DIR):
            if not name.endswith(".dispatching"):
                continue
            claimed = os.path.join(PENDING_INTAKE_DIR, name)
            pending = claimed[: -len(".dispatching")] + ".json"
            claimed_state = self._read(claimed)
            try:
                pending_state = self._read(pending)
            except FileNotFoundError:
                os.replace(claimed, pending)
                continue
            _atomic_write_json(pending, self._merge_states(claimed_state, pending_state))
            os.unlink(claimed)

    def _merge_shape(self, update):
        message = update["message"]
        update_id = update["update_id"]
        if update["update_kind"] not in (
            "message",
            "edited_message",
            "channel_post",
            "edited_channel_post",
        ):
            return f"single:{update_id}", 0
        media_group = message.get("media_group_id")
        text = message.get("text") or message.get("caption") or ""
        if media_group is not None:
            return f"media:{media_group}", self.settings["media_group_quiet_ms"]
        if message.get("photo") or (update["inbound"].get("kind") == "photo"):
            return (
                f"photo-burst:{update['update_kind']}",
                self.settings["photo_burst_quiet_ms"],
            )
        if text and not text.lstrip().startswith("/"):
            delay = self.settings["text_quiet_ms"]
            if text_units(text) >= self.settings["split_threshold_units"]:
                delay = self.settings["text_split_quiet_ms"]
            return f"text:{update['update_kind']}", delay
        return f"single:{update_id}", 0

    def _path(self, interface, key):
        digest = hashlib.sha256(f"{interface}\0{key}".encode("utf-8")).hexdigest()
        return os.path.join(PENDING_INTAKE_DIR, f"{digest}.json")

    def _read(self, path):
        with open(path, "r", encoding="utf-8") as handle:
            state = json.load(handle)
        state = _require_keys(
            state,
            {
                "schema_version",
                "interface",
                "merge_key",
                "deadline_unix_ms",
                "attempt",
                "updates",
            },
            set(),
            f"telegram pending intake {path}",
        )
        if state["schema_version"] != 1:
            raise ValueError(f"telegram pending intake {path} has unknown schema_version")
        _require_string(state["interface"], f"telegram pending intake {path}.interface")
        _require_string(state["merge_key"], f"telegram pending intake {path}.merge_key")
        _require_int(
            state["deadline_unix_ms"], f"telegram pending intake {path}.deadline_unix_ms", 0
        )
        _require_int(state["attempt"], f"telegram pending intake {path}.attempt", 0)
        if not isinstance(state["updates"], list) or not state["updates"]:
            raise ValueError(f"telegram pending intake {path}.updates must be non-empty")
        for index, update in enumerate(state["updates"]):
            update = _require_object(update, f"telegram pending intake {path}.updates[{index}]")
            _require_int(
                update.get("update_id"),
                f"telegram pending intake {path}.updates[{index}].update_id",
                0,
            )
        return state

    def submit(self, binding, update):
        if not self.settings["enabled"]:
            return self.hub.dispatch(binding, update)
        merge_key, delay_ms = self._merge_shape(update)
        now_ms = time.time_ns() // 1_000_000
        base_key = f"{binding['interface']}:{merge_key}"
        path = self._path(binding["interface"], base_key)
        with self.condition:
            try:
                state = self._read(path)
            except FileNotFoundError:
                state = {
                    "schema_version": 1,
                    "interface": binding["interface"],
                    "merge_key": merge_key,
                    "deadline_unix_ms": now_ms + delay_ms,
                    "attempt": 0,
                    "updates": [],
                }
            existing_ids = {item["update_id"] for item in state["updates"]}
            if update["update_id"] in existing_ids:
                return None
            projected_text = sum(
                len((item.get("inbound") or {}).get("text") or "")
                for item in state["updates"]
            ) + len((update.get("inbound") or {}).get("text") or "")
            if (
                len(state["updates"]) >= self.settings["max_items"]
                or projected_text > self.settings["max_text_chars"]
            ):
                base_key = f"{binding['interface']}:single:{update['update_id']}"
                path = self._path(binding["interface"], base_key)
                state = {
                    "schema_version": 1,
                    "interface": binding["interface"],
                    "merge_key": f"single:{update['update_id']}",
                    "deadline_unix_ms": now_ms,
                    "attempt": 0,
                    "updates": [],
                }
            state["updates"].append(update)
            state["updates"].sort(key=lambda item: item["update_id"])
            state["deadline_unix_ms"] = now_ms + delay_ms
            _atomic_write_json(path, state)
            self.condition.notify_all()
        return None

    def _pending(self):
        states = []
        for name in os.listdir(PENDING_INTAKE_DIR):
            if not name.endswith(".json"):
                continue
            path = os.path.join(PENDING_INTAKE_DIR, name)
            state = self._read(path)
            states.append((path, state))
        return states

    def _aggregate(self, state):
        updates = sorted(state["updates"], key=lambda item: item["update_id"])
        if len(updates) == 1:
            return updates[0]
        first = updates[0]
        last = updates[-1]
        chat_id = first["chat_id"]
        thread_id = first["thread_id"]
        if any(item["chat_id"] != chat_id or item["thread_id"] != thread_id for item in updates):
            raise ValueError("telegram pending batch crosses chat coordinates")
        messages = [item["message"] for item in updates]
        inbounds = [item["inbound"] for item in updates]
        message = dict(last["message"])
        texts = [item.get("text") for item in inbounds if item.get("text")]
        message["text"] = "\n".join(texts)
        message.pop("caption", None)
        media = []
        locations = []
        contacts = []
        for inbound in inbounds:
            media.extend(inbound.get("media") or [])
            if inbound.get("location") is not None:
                locations.append(inbound["location"])
            if inbound.get("contact") is not None:
                contacts.append(inbound["contact"])
        dispositions = {item.get("disposition", "respond") for item in inbounds}
        inbound = {
            "text": message["text"],
            "media": media,
            "kind": "media_group" if any(msg.get("media_group_id") for msg in messages) else "batch",
            "location": locations[-1] if locations else None,
            "contact": contacts[-1] if contacts else None,
            "disposition": "respond" if "respond" in dispositions else "observe",
            "batch_count": len(updates),
            "event": {
                "update_ids": [item["update_id"] for item in updates],
                "update_kinds": [item["update_kind"] for item in updates],
                "messages": messages,
                "locations": locations,
                "contacts": contacts,
            },
        }
        return {
            "update_id": last["update_id"],
            "update_kind": "batch",
            "chat_id": chat_id,
            "thread_id": thread_id,
            "message": message,
            "inbound": inbound,
        }

    def _run(self):
        while True:
            try:
                now_ms = time.time_ns() // 1_000_000
                with self.condition:
                    pending = self._pending()
                by_interface = {}
                for path, state in pending:
                    first_id = min(item["update_id"] for item in state["updates"])
                    by_interface.setdefault(state["interface"], []).append(
                        (first_id, path, state)
                    )
                progressed = False
                for interface, entries in by_interface.items():
                    _first_id, path, state = min(entries, key=lambda item: item[0])
                    with self.condition:
                        try:
                            state = self._read(path)
                        except FileNotFoundError:
                            continue
                        if state["deadline_unix_ms"] > now_ms:
                            continue
                        binding = _binding_by_interface(interface)
                        if binding is None:
                            raise RuntimeError(
                                f"telegram pending intake references unknown interface {interface!r}"
                            )
                        claimed = path[: -len(".json")] + ".dispatching"
                        os.replace(path, claimed)
                    try:
                        self.hub.dispatch(binding, self._aggregate(state))
                    except Exception as error:
                        state["attempt"] += 1
                        state["deadline_unix_ms"] = now_ms + min(
                            self.config["driver_ipc"]["reconnect_backoff_cap_ms"],
                            self.config["driver_ipc"]["reconnect_backoff_ms"]
                            * (2 ** min(state["attempt"], 10)),
                        )
                        with self.condition:
                            try:
                                pending_state = self._read(path)
                            except FileNotFoundError:
                                _atomic_write_json(path, state)
                            else:
                                _atomic_write_json(path, self._merge_states(state, pending_state))
                            os.unlink(claimed)
                        sys.stderr.write(
                            f"telegram pending intake delivery retry for {interface}: {sanitize_transport_error(error)}\n"
                        )
                        sys.stderr.flush()
                        continue
                    os.unlink(claimed)
                    progressed = True
                if progressed:
                    continue
            except Exception as error:
                sys.stderr.write(f"telegram pending intake stalled: {sanitize_transport_error(error)}\n")
                sys.stderr.flush()
            with self.condition:
                self.condition.wait(0.05)


def dispatch_gateway_update(binding, update):
    if _INTAKE_BATCHER is None:
        raise RuntimeError("telegram intake batcher is not initialized")
    return _INTAKE_BATCHER.submit(binding, update)


def _child_life_json(secret_handle):
    runtime_source = os.environ.get("OCTOPUS_CONTENT_HASH_RUNTIME")
    if not isinstance(runtime_source, str) or not os.path.isabs(runtime_source):
        raise RuntimeError("telegram child topology requires the package-owned content-hash runtime")
    runtime_source = os.path.realpath(runtime_source)
    return {
        "processes": [
            {
                "bin": "python3",
                "io_limits": {
                    "max_control_frame_bytes": 1048576,
                    "max_stderr_bytes": 262144,
                    "max_manifest_bytes": 1048576,
                    "max_payload_bytes": 268435456,
                },
                "args": [
                    {"repository_file": "registry/interface/world_telegram/octopus_python_boot.py"},
                    {"repository_file": "registry/interface/world_telegram/telegram_life.py"}
                ],
                "required_files": [
                    "registry/interface/world_telegram/octopus_python_boot.py",
                    "registry/interface/world_telegram/telegram_life.py",
                    "registry/interface/world_telegram/telegram_rendering.py",
                    "registry/interface/world_telegram/octopus_content_hash.py",
                    "registry/interface/world_telegram/requirements-content-hash.lock",
                ],
                "env": {"OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE": runtime_source},
                "inherit_env": [
                    "OCTOPUS_TELEGRAM_API_BASE",
                    "OCTOPUS_TELEGRAM_BOT_USERNAME",
                    "OCTOPUS_TELEGRAM_FALLBACK_IPS",
                    "PATH",
                    "TELEGRAM_LOCAL",
                    "TELEGRAM_LOCAL_FILES_ROOTS",
                    "TELEGRAM_PROXY",
                    "TELEGRAM_BOT_TOKEN",
                ],
                "authority": {
                    "read_root_slots": [],
                    "write_root_slots": [],
                    "executable_root_slots": [],
                    "read_paths": [secret_handle, runtime_source],
                    "write_paths": [],
                    "executable_paths": [runtime_source],
                    "executable_commands": ["python3"],
                    "read_env_paths": [],
                    "write_env_paths": [],
                    "executable_env_paths": [],
                    "optional_read_env_paths": ["TELEGRAM_LOCAL_FILES_ROOTS"],
                    "allow_network": True,
                    "allow_local_ipc": True,
                },
                "read_handles": ["ledger", "blobs", "ledger_index"],
            }
        ]
    }


def _registry_owner_document(config, root_slot, owner, filename, label):
    owner = _require_string(owner, f"telegram {label} owner")
    if os.path.basename(owner) != owner or owner in (".", ".."):
        raise ValueError(f"telegram {label} owner must be one registry folder name")
    if os.path.basename(filename) != filename or filename in (".", ".."):
        raise ValueError(f"telegram {label} owner file is unsupported")
    path = os.path.join(_root_slots()[root_slot], owner, filename)
    try:
        raw = _read_bounded_regular(
            path,
            config["driver_ipc"]["max_manifest_bytes"],
            f"{label} owner {owner} {filename}",
        )
    except FileNotFoundError as error:
        raise RuntimeError(
            f"telegram topology requires installed {label} owner {owner!r} with {filename}"
        ) from error
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise ValueError(
            f"telegram {label} owner {owner!r} {filename} is malformed: {error}"
        ) from error
    if not isinstance(value, dict):
        raise ValueError(
            f"telegram {label} owner {owner!r} {filename} must be an object"
        )
    return path, raw, value


def _worker_role_artifact_bindings(config, worker, policy=None):
    worker = _require_string(worker, "telegram exact worker interface")
    if policy is None:
        policy = _registry_owner_document(
            config,
            "registry_interface_root",
            worker,
            "policy.json",
            "worker interface",
        )[2]
    policy = _require_object(policy, "telegram exact worker policy")
    if policy.get("id") != worker:
        raise ValueError("telegram exact worker policy identity does not match its registry folder")
    roles = _require_string_list(
        policy.get("role_artifacts"),
        "telegram exact worker policy.role_artifacts",
        True,
    )
    return [{"id": artifact} for artifact in roles]


def _interface_owner_json(config, owner, filename):
    if filename not in ("interface.json", "life.json"):
        raise ValueError("telegram interface owner file is unsupported")
    return _registry_owner_document(
        config,
        "registry_interface_root",
        owner,
        filename,
        "interface",
    )[2]


def _exact_interface_descriptor(config, interface, owner):
    descriptor = _interface_owner_json(config, owner, "interface.json")
    if descriptor.get("id") != owner:
        raise ValueError(
            f"telegram interface owner {owner!r} descriptor declares mismatched id {descriptor.get('id')!r}"
        )
    if "reference" in descriptor:
        raise ValueError(f"telegram interface owner {owner!r} still declares forbidden reference")
    interface = _require_string(interface, "telegram exact interface id")
    if os.path.basename(interface) != interface or interface in (".", ".."):
        raise ValueError("telegram exact interface id must be one registry folder name")
    descriptor["id"] = interface
    return descriptor


def _interface_owner_life(config, owner):
    life = _interface_owner_json(config, owner, "life.json")
    processes = life.get("processes")
    if not isinstance(processes, list) or not processes:
        raise ValueError(f"telegram interface owner {owner!r} life declares no processes")
    return life


def _exact_tool_descriptor(config, identifier, interface):
    private = config["private_run"]
    owner = private["file_write_tool"]
    _, _, descriptor = _registry_owner_document(
        config,
        "registry_tool_root",
        owner,
        "tool.json",
        "tool",
    )
    if descriptor.get("id") != owner:
        raise ValueError(f"telegram tool owner {owner!r} declares mismatched id")
    if "reference" in descriptor:
        raise ValueError(f"telegram tool owner {owner!r} still declares forbidden reference")
    if descriptor.get("interface") != private["worker_environment_door_owner"]:
        raise ValueError("telegram file-write tool owner is not owned by the configured door owner")
    kinds = _require_string_list(
        descriptor.get("supported_worker_environment_kinds"),
        "telegram file-write tool owner supported_worker_environment_kinds",
    )
    if private["worker_environment_kind"] not in kinds:
        raise ValueError("telegram file-write tool owner does not support the configured worker environment kind")
    if not isinstance(descriptor.get("process"), dict):
        raise ValueError("telegram file-write tool owner declares no process object")
    if not isinstance(descriptor.get("args_schema"), dict):
        raise ValueError("telegram file-write tool owner declares no args_schema object")
    identifier = _require_string(identifier, "telegram exact file-write tool")
    interface = _require_string(interface, "telegram exact worker-environment interface")
    if os.path.basename(identifier) != identifier or identifier in (".", ".."):
        raise ValueError("telegram exact file-write tool must be one registry folder name")
    if os.path.basename(interface) != interface or interface in (".", ".."):
        raise ValueError("telegram exact worker-environment interface must be one registry folder name")
    descriptor["id"] = identifier
    descriptor["interface"] = interface
    return descriptor


def _ee_template_owner_document(config, template):
    path, raw, descriptor = _registry_owner_document(
        config,
        "registry_ee_template_root",
        template,
        "ee_template.json",
        "EE template",
    )
    if descriptor.get("id") != template:
        raise ValueError(
            f"telegram EE template {template!r} declares mismatched id {descriptor.get('id')!r}"
        )
    return path, raw, descriptor


def _parent_ee_chain(config):
    current = config["private_run"]["parent_ee"]
    seen_ees = set()
    chain = []
    while current is not None:
        current = _require_string(current, "telegram parent EE")
        if current in seen_ees:
            raise ValueError("telegram parent EE chain contains a cycle")
        seen_ees.add(current)
        path = os.path.join(_root_slots()["ee_instances_root"], f"{current}.json")
        try:
            raw = _read_bounded_regular(
                path,
                config["driver_ipc"]["max_manifest_bytes"],
                f"parent EE {current}",
            )
        except FileNotFoundError as error:
            raise RuntimeError(
                f"telegram topology requires installed parent EE {current!r}"
            ) from error
        try:
            instance = json.loads(raw)
        except json.JSONDecodeError as error:
            raise ValueError(f"telegram parent EE {current!r} is malformed: {error}") from error
        if not isinstance(instance, dict) or instance.get("ee_id") != current:
            raise ValueError(f"telegram parent EE {current!r} has mismatched identity")
        template = _require_string(instance.get("template_name"), f"telegram parent EE {current!r}.template_name")
        if template not in chain:
            chain.append(template)
        current = instance.get("parent_ee_id")
    if not chain:
        raise ValueError("telegram private topology has no parent EE chain")
    return chain


def _merge_string_list(document, key, additions, label):
    current = document.get(key)
    if not isinstance(current, list) or any(
        not isinstance(value, str) or not value for value in current
    ):
        raise ValueError(f"telegram {label}.{key} must be a string array")
    merged = list(current)
    seen = set(current)
    for value in additions:
        if not isinstance(value, str) or not value:
            raise ValueError(f"telegram {label}.{key} addition must be a non-empty string")
        if value not in seen:
            merged.append(value)
            seen.add(value)
    document[key] = merged


def _require_optional_axis_admits(document, key, value, label):
    current = document.get(key)
    if current is None:
        return
    if not isinstance(current, list) or any(
        not isinstance(item, str) or not item for item in current
    ):
        raise ValueError(f"telegram {label}.{key} must be null or a string array")
    if value not in current:
        raise ValueError(f"telegram {label}.{key} does not admit {value!r}")


def _exact_ee_template_id(config, binding):
    return f"{config['private_run']['id_prefixes']['ee_template']}{binding['opaque_id']}"


def _binding_policy_writes(config, binding):
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
    parent_templates = _parent_ee_chain(config)
    for index, parent_template in enumerate(parent_templates):
        parent_path, parent_raw, parent = _ee_template_owner_document(config, parent_template)
        _require_optional_axis_admits(
            parent,
            "allowed_worker_environment_kinds",
            config["private_run"]["worker_environment_kind"],
            f"EE template {parent_template}",
        )
        _merge_string_list(parent, "allowed_interfaces", exact_interfaces, f"EE template {parent_template}")
        if parent.get("allowed_tools") is not None:
            _merge_string_list(parent, "allowed_tools", [tool], f"EE template {parent_template}")
        if index == 0:
            _merge_string_list(
                parent,
                "allowed_child_ee_templates",
                [_exact_ee_template_id(config, binding)],
                f"EE template {parent_template}",
            )
        parent_content = _json_text(parent)
        if parent_content.encode("utf-8") != parent_raw:
            writes.append(
                {
                    "path": parent_path,
                    "content": parent_content,
                    "expected_before_hash": octopus_content_hash(parent_raw),
                }
            )
    return sorted(writes, key=lambda write: write["path"])


def _wait_for_shared_policy_admissions(config, binding, attempted_hashes):
    deadline = time.monotonic() + config["private_run"]["result_timeout_ms"] / 1000.0
    while True:
        pending = _binding_policy_writes(config, binding)
        if not pending:
            return True
        if any(
            attempted_hashes.get(write["path"]) != write["expected_before_hash"]
            for write in pending
        ):
            return False
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        time.sleep(min(0.05, remaining))


def _binding_topology(config, binding):
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
    ee_template_id = _exact_ee_template_id(config, binding)
    checkpoint_prefix = f"cp_{binding['opaque_id'][:12]}"
    child_config = json.loads(json.dumps(config))
    child_config["mode"] = "binding"
    child_config.pop("gateway_run_template", None)
    secret_handle = _ensure_binding_secret(binding)[0]
    child_config["binding"] = {
        "interface": interface,
        "chat_id": binding["chat_id"],
        "thread_id": binding["thread_id"],
        "gateway_interface": INTERFACE_ID,
        "secret_handle": secret_handle,
        "host": binding["host"],
        "port": binding["port"],
        "run": binding["run"],
    }
    telegram_descriptor = _exact_interface_descriptor(
        config, interface, owners["telegram"]
    )
    provider_descriptor = _exact_interface_descriptor(
        config, provider, owners["provider"]
    )
    provider_driver_config = provider_descriptor.get("interface_driver_config")
    if not isinstance(provider_driver_config, dict):
        raise ValueError("telegram provider owner declares no interface_driver_config object")
    provider_metadata = provider_driver_config.get("private_config")
    if not isinstance(provider_metadata, dict):
        raise ValueError("telegram provider owner declares no private_config object")
    provider_metadata.pop("run_authoring", None)
    provider_metadata.update(
        {
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
    )
    authoring_source_descriptor = _exact_interface_descriptor(
        config, authoring_source, owners["interface_authoring"]
    )
    worker_descriptor = _exact_interface_descriptor(
        config, worker, owners["worker"]
    )
    door_descriptor = _exact_interface_descriptor(
        config,
        authoring,
        private["worker_environment_door_owner"],
    )
    door_life = _interface_owner_life(
        config, private["worker_environment_door_owner"]
    )
    tool_descriptor = _exact_tool_descriptor(config, file_write_tool, authoring)
    provider_life = _interface_owner_life(config, owners["provider"])
    authoring_source_life = _interface_owner_life(
        config, owners["interface_authoring"]
    )
    _, _, ee_template = _ee_template_owner_document(config, private["ee_template"])
    ee_template["id"] = ee_template_id
    _require_optional_axis_admits(
        ee_template,
        "allowed_worker_environment_kinds",
        private["worker_environment_kind"],
        f"EE template {ee_template_id}",
    )
    _merge_string_list(
        ee_template,
        "allowed_interfaces",
        [interface, provider, authoring_source, authoring, worker],
        f"EE template {ee_template_id}",
    )
    if ee_template.get("allowed_tools") is not None:
        allowed_tools = _require_string_list(
            ee_template["allowed_tools"],
            f"EE template {ee_template_id}.allowed_tools",
            True,
        )
        if private["file_write_tool"] not in allowed_tools:
            raise ValueError("telegram EE template owner does not admit the file-write tool owner")
        ee_template["allowed_tools"] = list(
            dict.fromkeys(
                file_write_tool if tool == private["file_write_tool"] else tool
                for tool in allowed_tools
            )
        )
    worker_policy = {
        "id": worker,
        "ceiling": {
            "interfaces": [
                interface,
                provider,
                authoring_source,
                authoring,
                worker,
            ],
            "tools": [file_write_tool],
            "wek": [private["worker_environment_kind"]],
            "mark_outcomes": [private["file_write_mark_outcome"]],
            "hard_blocked_tools": [],
        },
        "role_artifacts": private["role_artifacts"],
    }
    mandatory_role_bindings = _worker_role_artifact_bindings(config, worker, worker_policy)
    context_artifacts = [binding["id"] for binding in mandatory_role_bindings]
    for route in private["context_routes"]:
        chat_match = not route["chat_ids"] or binding["chat_id"] in route["chat_ids"]
        thread_match = not route["thread_ids"] or binding["thread_id"] in route["thread_ids"]
        if chat_match and thread_match:
            for artifact in route["artifact_bindings"]:
                if artifact not in context_artifacts:
                    context_artifacts.append(artifact)
    context_bindings = [{"id": artifact} for artifact in context_artifacts]
    run_record = {
        "id": run,
        "ee": ee,
        "worker": worker,
        "trace": f"trace:{run}",
        "status": "running",
        "interface_state": {
            provider: {
                "context_generation": 0,
                "authoring_generation": 0,
                "frontier_checkpoint_ids": [
                    f"{checkpoint_prefix}_telegram_output",
                    f"{checkpoint_prefix}_authoring_call",
                    f"{checkpoint_prefix}_authoring_result",
                ],
            }
        },
        "checkpoints": [
            {
                "id": f"{checkpoint_prefix}_telegram_input",
                "run": run,
                "trace": f"trace:{run}:{checkpoint_prefix}_telegram_input",
                "epoch": 0,
                "interface_binding": _authored_interface_binding(interface),
                "outbound_interface_bindings": [_authored_interface_binding(provider)],
                "artifact_bindings": context_bindings,
                "required_verified_artifacts": [],
            },
            {
                "id": f"{checkpoint_prefix}_telegram_output",
                "run": run,
                "trace": f"trace:{run}:{checkpoint_prefix}_telegram_output",
                "epoch": 0,
                "interface_binding": _authored_interface_binding(provider),
                "outbound_interface_bindings": [_authored_interface_binding(interface)],
                "artifact_bindings": mandatory_role_bindings,
                "required_verified_artifacts": [],
            },
            {
                "id": f"{checkpoint_prefix}_authoring_call",
                "run": run,
                "trace": f"trace:{run}:{checkpoint_prefix}_authoring_call",
                "epoch": 0,
                "interface_binding": _authored_interface_binding(authoring_source),
                "outbound_interface_bindings": [_authored_interface_binding(authoring)],
                "artifact_bindings": mandatory_role_bindings,
                "required_verified_artifacts": [],
            },
            {
                "id": f"{checkpoint_prefix}_authoring_result",
                "run": run,
                "trace": f"trace:{run}:{checkpoint_prefix}_authoring_result",
                "epoch": 0,
                "interface_binding": _authored_interface_binding(authoring),
                "outbound_interface_bindings": [_authored_interface_binding(provider)],
                "artifact_bindings": mandatory_role_bindings,
                "required_verified_artifacts": [],
            },
        ],
        "active_checkpoints": [
            f"{checkpoint_prefix}_telegram_input",
            f"{checkpoint_prefix}_telegram_output",
            f"{checkpoint_prefix}_authoring_call",
            f"{checkpoint_prefix}_authoring_result",
        ],
    }
    activation = {
        "schema_version": 1,
        "interface": authoring_source,
        "activations": [
            {
                "activation_driver_ref": "app_server_transport",
                "config": {"bind_interface_socket_when_env_unset": True},
            }
        ],
    }
    ee_instance = {
        "ee_id": ee,
        "template_name": ee_template_id,
        "parent_ee_id": private["parent_ee"],
        "goal": f"Serve the Telegram chat bound to interface {interface}",
    }
    roots = _root_slots()
    interface_root = roots["registry_interface_root"]
    return {
        os.path.join(interface_root, interface, "config.json"): _json_text(child_config),
        os.path.join(interface_root, interface, "life.json"): _json_text(_child_life_json(secret_handle)),
        os.path.join(interface_root, interface, "interface.json"): _json_text(telegram_descriptor),
        os.path.join(interface_root, provider, "life.json"): _json_text(provider_life),
        os.path.join(interface_root, provider, "interface.json"): _json_text(provider_descriptor),
        os.path.join(interface_root, authoring_source, "activation.json"): _json_text(activation),
        os.path.join(interface_root, authoring_source, "life.json"): _json_text(authoring_source_life),
        os.path.join(interface_root, authoring_source, "interface.json"): _json_text(authoring_source_descriptor),
        os.path.join(interface_root, authoring, "life.json"): _json_text(door_life),
        os.path.join(interface_root, authoring, "interface.json"): _json_text(door_descriptor),
        os.path.join(roots["registry_tool_root"], file_write_tool, "tool.json"): _json_text(tool_descriptor),
        os.path.join(interface_root, worker, "policy.json"): _json_text(worker_policy),
        os.path.join(interface_root, worker, "interface.json"): _json_text(worker_descriptor),
        os.path.join(roots["registry_ee_template_root"], ee_template_id, "ee_template.json"): _json_text(ee_template),
        os.path.join(roots["ee_instances_root"], f"{ee}.json"): _json_text(ee_instance),
        os.path.join(roots["run_records_root"], f"{run}.json"): _json_text(run_record),
    }


def _ensure_gateway_run(config):
    template = config.get("gateway_run_template")
    if not isinstance(template, dict):
        raise RuntimeError("telegram gateway config declares no gateway_run_template object")
    run_id = _require_string(template.get("id"), "telegram gateway_run_template.id")
    path = os.path.join(_root_slots()["run_records_root"], f"{run_id}.json")
    try:
        existing = _read_bounded_regular(
            path,
            config["driver_ipc"]["max_manifest_bytes"],
            "gateway run",
        )
    except FileNotFoundError as error:
        raise RuntimeError(
            f"telegram gateway run {path!r} is not authored; provision gateway_run_template before enabling the interface"
        ) from error
    record = json.loads(existing)
    if not isinstance(record, dict):
        raise RuntimeError(f"telegram gateway run {path!r} is not a JSON object")
    if (
        record.get("id") != run_id
        or record.get("ee") != template.get("ee")
        or record.get("worker") != template.get("worker")
    ):
        raise RuntimeError(
            f"telegram gateway run {path!r} identity conflicts with the declared gateway_run_template"
        )
    configured_door = config["private_run"]["gateway_worker_environment_interface"]
    if not any(
        _require_interface_binding(checkpoint.get("interface_binding"), f"telegram gateway checkpoint {checkpoint.get('id')}.interface_binding") == configured_door
        or configured_door in [
            _require_interface_binding(binding, f"telegram gateway checkpoint {checkpoint.get('id')}.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(checkpoint.get("outbound_interface_bindings", []))
        ]
        for checkpoint in record.get("checkpoints", [])
        if isinstance(checkpoint, dict)
    ):
        raise RuntimeError("telegram gateway run is not bound to its exact worker-environment interface")
    return path


def _gateway_rearm_write(config, opaque_id):
    template = config["gateway_run_template"]
    run_id = template["id"]
    worker = template["worker"]
    authoring = config["private_run"]["gateway_worker_environment_interface"]
    path = _ensure_gateway_run(config)
    current = _read_bounded_regular(
        path,
        config["driver_ipc"]["max_manifest_bytes"],
        "gateway run",
    )
    try:
        record = json.loads(current)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"telegram gateway run {path!r} is malformed: {error}") from error
    if record.get("id") != run_id or record.get("worker") != worker:
        raise RuntimeError("telegram gateway activation run identity drifted")
    mandatory_role_bindings = _worker_role_artifact_bindings(config, worker)
    interface_state = record.get("interface_state")
    if not isinstance(interface_state, dict):
        raise RuntimeError("telegram gateway run interface_state must be an object")
    state = interface_state.setdefault(INTERFACE_ID, {})
    if not isinstance(state, dict):
        raise RuntimeError("telegram gateway interface state must be an object")
    previous = state.get("authoring_generation", 0)
    if isinstance(previous, bool) or not isinstance(previous, int) or previous < 0:
        raise RuntimeError("telegram gateway authoring_generation must be unsigned")
    generation = previous + 1
    state["authoring_generation"] = generation
    suffix = f"{opaque_id[:12]}_{generation}"
    call = f"cp_telegram_gateway_authoring_call_{suffix}"
    result = f"cp_telegram_gateway_authoring_result_{suffix}"
    record["checkpoints"] = [
        {
            "id": call,
            "run": run_id,
            "trace": f"trace:{run_id}:{call}",
            "epoch": 0,
            "interface_binding": _authored_interface_binding(INTERFACE_ID),
            "outbound_interface_bindings": [_authored_interface_binding(authoring)],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": [],
        },
        {
            "id": result,
            "run": run_id,
            "trace": f"trace:{run_id}:{result}",
            "epoch": 0,
            "interface_binding": _authored_interface_binding(authoring),
            "outbound_interface_bindings": [_authored_interface_binding(INTERFACE_ID)],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": [],
        },
    ]
    record["active_checkpoints"] = [call, result]
    return {
        "path": path,
        "content": _json_text(record),
        "expected_before_hash": octopus_content_hash(current),
    }


def _missing_topology_writes(config, topology):
    ownership = _load_topology_ownership()
    writes = []
    for path, content in topology.items():
        desired = content.encode("utf-8")
        try:
            existing = _read_bounded_regular(
                path,
                config["driver_ipc"]["max_manifest_bytes"],
                "topology document",
            )
        except FileNotFoundError:
            if path in ownership["paths"]:
                raise RuntimeError(f"telegram package-authored topology target {path!r} disappeared")
            writes.append({"path": path, "content": content, "expected_before_hash": None})
            continue
        if existing == desired:
            continue
        observed = octopus_content_hash(existing)
        if ownership["paths"].get(path) != observed:
            raise RuntimeError(f"telegram private topology target {path!r} drifted from its last package-authored bytes")
        writes.append({"path": path, "content": content, "expected_before_hash": observed})
    return writes


def _load_topology_ownership():
    try:
        state = json.loads(
            _read_bounded_regular(
                TOPOLOGY_OWNERSHIP_PATH,
                TOPOLOGY_MAX_BYTES,
                "topology ownership",
            )
        )
    except FileNotFoundError:
        return {"schema_version": 1, "paths": {}}
    if not isinstance(state, dict) or set(state) != {"schema_version", "paths"}:
        raise ValueError("telegram topology ownership must contain schema_version and paths")
    if state["schema_version"] != 1 or not isinstance(state["paths"], dict):
        raise ValueError("telegram topology ownership has an unsupported shape")
    if len(state["paths"]) > TOPOLOGY_OWNERSHIP_MAX_PATHS:
        raise ValueError("telegram topology ownership has too many paths")
    for path, digest in state["paths"].items():
        _require_string(path, "telegram topology ownership path")
        if not os.path.isabs(path):
            raise ValueError("telegram topology ownership path must be absolute")
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError("telegram topology ownership digest is malformed")
    return state


def _commit_topology_ownership(config, topology):
    ownership = _load_topology_ownership()
    for path, content in sorted(topology.items()):
        desired = content.encode("utf-8")
        existing = _read_bounded_regular(
            path,
            config["driver_ipc"]["max_manifest_bytes"],
            "topology document",
        )
        if existing != desired:
            raise RuntimeError(f"telegram package-authored topology target {path!r} did not materialize exactly")
        ownership["paths"][path] = octopus_content_hash(existing)
    if len(ownership["paths"]) > TOPOLOGY_OWNERSHIP_MAX_PATHS:
        raise ValueError("telegram topology ownership has too many paths")
    _atomic_write_json(TOPOLOGY_OWNERSHIP_PATH, ownership, indent=2)


def _package_owned_topology(topology):
    slots = _root_slots()
    roots = (
        slots["registry_interface_root"],
        slots["registry_ee_template_root"],
        slots["registry_tool_root"],
    )
    return {
        path: content
        for path, content in topology.items()
        if any(
            os.path.commonpath((os.path.realpath(path), root)) == root
            and os.path.realpath(path) != root
            for root in roots
        )
    }


def ensure_private_binding(config, hub, chat_id, thread_id):
    with _AUTHORING_LOCK:
        binding = _ensure_binding_record(config, chat_id, thread_id, hub.host, hub.port)
        for attempt in range(config["private_run"]["authoring_max_attempts"]):
            topology = _binding_topology(config, binding)
            managed_topology = _package_owned_topology(topology)
            topology_writes = _missing_topology_writes(
                config,
                managed_topology if binding["status"] == "ready" else topology,
            )
            shared_writes = _binding_policy_writes(config, binding)
            writes = topology_writes + shared_writes
            if writes:
                writes.insert(0, _gateway_rearm_write(config, binding["opaque_id"]))
                now = time.time_ns()
                payload = json.dumps(
                    {
                        "tool": config["private_run"]["gateway_file_write_tool"],
                        "arguments": {"writes": writes},
                        "correlation_id": f"telegram-binding-authoring:{binding['opaque_id']}:{now}",
                    },
                    sort_keys=True,
                ).encode("utf-8")
                hand_artifact(
                    materialize_runtime_artifact(
                        config["private_run"]["file_write_label"],
                        payload,
                        f"telegram-binding-authoring-{binding['opaque_id']}-{now}",
                    )
                )
            if not _wait_for_shared_policy_admissions(
                config,
                binding,
                {
                    write["path"]: write["expected_before_hash"]
                    for write in shared_writes
                },
            ):
                if attempt + 1 < config["private_run"]["authoring_max_attempts"]:
                    continue
                raise RuntimeError(
                    "telegram exact parent and worker admissions could not be authored without overwriting concurrent ids"
                )
            try:
                hub.wait_for(
                    binding["interface"], config["driver_ipc"]["connect_timeout_ms"]
                )
            except TimeoutError:
                if attempt + 1 < config["private_run"]["authoring_max_attempts"]:
                    continue
                raise
            _commit_topology_ownership(config, managed_topology)
            if binding["status"] != "ready":
                _mark_binding_ready(binding)
            return binding
        raise RuntimeError("telegram private topology authoring exhausted its attempts")


def _reconcile_known_bindings(config, hub):
    with _STATE_LOCK:
        coordinates = [
            (binding["chat_id"], binding["thread_id"])
            for _, binding in sorted(_load_bindings()["bindings"].items())
        ]
    for chat_id, thread_id in coordinates:
        ensure_private_binding(config, hub, chat_id, thread_id)


def validate_chat_coordinates(chat_id, thread_id):
    if isinstance(chat_id, bool) or not isinstance(chat_id, int):
        raise ValueError("telegram chat_id must be an integer")
    if chat_id == 0:
        raise ValueError("telegram chat_id must not be zero")
    if thread_id is not None and (
        isinstance(thread_id, bool) or not isinstance(thread_id, int) or thread_id <= 0
    ):
        raise ValueError("telegram thread_id must be a positive integer or null")


def chat_state_path(chat_id, thread_id):
    validate_chat_coordinates(chat_id, thread_id)
    key = str(chat_id) if thread_id is None else f"{chat_id}:{thread_id}"
    safe = key.replace("/", "_").replace(":", "__")
    return os.path.join(CHAT_STATES_DIR, f"{safe}.json")


def read_chat_state(chat_id, thread_id):
    path = chat_state_path(chat_id, thread_id)
    with _STATE_LOCK:
        with open(path, "r") as handle:
            state = json.load(handle)
        if not isinstance(state, dict):
            raise ValueError(f"telegram chat state {path!r} is not an object")
        if state.get("chat_id") != chat_id or state.get("thread_id") != thread_id:
            raise ValueError(f"telegram chat state {path!r} has mismatched coordinates")
        return state


def load_or_create_chat_state(chat_id, thread_id):
    with _STATE_LOCK:
        try:
            return read_chat_state(chat_id, thread_id)
        except FileNotFoundError:
            pass
        path = chat_state_path(chat_id, thread_id)
        state = {
            "chat_id": chat_id,
            "thread_id": thread_id,
            "last_bot_message_id": None,
            "last_user_message_id": None,
            "callbacks": {},
            "stream_message_id": None,
            "stream_last_edit_ms": 0,
            "stream_text": "",
            "stream_last_seq": None,
            "stream_draft_id": None,
            "last_update_id": None,
            "sent_messages": {},
        }
        _atomic_write_json(path, state)
        return state


def update_chat_state(chat_id, thread_id, **fields):
    with _STATE_LOCK:
        path = chat_state_path(chat_id, thread_id)
        state = read_chat_state(chat_id, thread_id)
        state.update(fields)
        _atomic_write_json(path, state)
        return state


def remember_sent_message(chat_id, thread_id, message_id, text):
    if message_id is None or not text:
        return
    with _STATE_LOCK:
        state = read_chat_state(chat_id, thread_id)
        sent = state.get("sent_messages", {})
        if not isinstance(sent, dict):
            raise ValueError("telegram chat state sent_messages must be an object")
        sent[str(message_id)] = text
        while len(sent) > 128:
            sent.pop(next(iter(sent)))
        state["sent_messages"] = sent
        _atomic_write_json(chat_state_path(chat_id, thread_id), state)


def enrich_reply_reference(chat_id, thread_id, reply):
    if not isinstance(reply, dict):
        return reply
    if reply.get("text") or reply.get("caption"):
        return reply
    message_id = reply.get("message_id")
    if message_id is None:
        return reply
    state = load_or_create_chat_state(chat_id, thread_id)
    sent = state.get("sent_messages", {})
    if not isinstance(sent, dict):
        raise ValueError("telegram chat state sent_messages must be an object")
    recovered = sent.get(str(message_id))
    if not recovered:
        return reply
    enriched = dict(reply)
    enriched["text"] = recovered
    enriched["recovered_from_interface_state"] = True
    return enriched


def load_pairing():
    try:
        state = json.loads(
            _read_bounded_regular(
                PAIRING_PATH,
                PAIRING_MAX_STATE_BYTES,
                "pairing state",
            )
        )
    except FileNotFoundError:
        return {"schema_version": 2, "approved": [], "pending": {}}
    if not isinstance(state, dict) or set(state) != {"schema_version", "approved", "pending"}:
        raise ValueError("telegram pairing state has an invalid shape")
    if state["schema_version"] != 2:
        raise ValueError("telegram pairing state schema_version must be 2")
    approved = state["approved"]
    pending = state["pending"]
    if not isinstance(approved, list) or any(
        isinstance(user_id, bool) or not isinstance(user_id, int) for user_id in approved
    ):
        raise ValueError("telegram pairing approved must be an integer array")
    if len(approved) > PAIRING_MAX_APPROVED:
        raise ValueError("telegram pairing approved exceeds its finite bound")
    if not isinstance(pending, dict):
        raise ValueError("telegram pairing pending must be an object")
    if len(pending) > PAIRING_MAX_PENDING:
        raise ValueError("telegram pairing pending exceeds its finite bound")
    for code, entry in pending.items():
        if not re.fullmatch(r"TG-[0-9]{6}", code):
            raise ValueError(f"telegram pairing state has malformed code {code!r}")
        entry = _require_keys(
            entry,
            {"user_id", "created_at_unix_ms"},
            set(),
            f"telegram pairing state code {code!r}",
        )
        _require_int(entry["user_id"], f"telegram pairing state code {code!r}.user_id")
        _require_int(entry["created_at_unix_ms"], f"telegram pairing state code {code!r}.created_at_unix_ms", 0)
    return state


def prune_pairing(state, now_ms):
    expired = [
        code
        for code, entry in state["pending"].items()
        if entry["created_at_unix_ms"] + PAIRING_TTL_MS <= now_ms
    ]
    for code in expired:
        state["pending"].pop(code)
    return bool(expired)


def commit_pairing(state):
    payload = json.dumps(state, ensure_ascii=False, sort_keys=True).encode("utf-8")
    if len(payload) > PAIRING_MAX_STATE_BYTES:
        raise RuntimeError("telegram pairing state exceeds its finite byte bound")
    _atomic_write_bytes(PAIRING_PATH, payload)


def register_pending_pairing(user_id, now_ms=None):
    if isinstance(user_id, bool) or not isinstance(user_id, int):
        raise ValueError("telegram pairing requires an integer user id")
    now_ms = int(time.time() * 1000) if now_ms is None else _require_int(now_ms, "telegram pairing time", 0)
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
            raise RuntimeError("telegram pairing pending capacity is exhausted")
        for _attempt in range(100):
            code = f"TG-{secrets.randbelow(1000000):06d}"
            if code not in state["pending"]:
                state["pending"][code] = {"user_id": user_id, "created_at_unix_ms": now_ms}
                commit_pairing(state)
                return code
    raise RuntimeError("telegram pairing code space is unexpectedly saturated")


def approve_pairing(code, now_ms=None):
    code = _require_string(code, "telegram pairing approval code")
    if not re.fullmatch(r"TG-[0-9]{6}", code):
        raise ValueError("telegram pairing approval code is malformed")
    now_ms = int(time.time() * 1000) if now_ms is None else _require_int(now_ms, "telegram pairing time", 0)
    with _STATE_LOCK:
        state = load_pairing()
        prune_pairing(state, now_ms)
        entry = state["pending"].pop(code, None)
        if entry is None:
            commit_pairing(state)
            raise ValueError("telegram pairing approval code is absent or expired")
        user_id = entry["user_id"]
        if user_id not in state["approved"]:
            if len(state["approved"]) >= PAIRING_MAX_APPROVED:
                raise RuntimeError("telegram pairing approved capacity is exhausted")
            state["approved"].append(user_id)
        commit_pairing(state)
        return {"status": "approved", "user_id": user_id}


def revoke_pairing(user_id):
    user_id = _require_int(user_id, "telegram pairing revoke user")
    with _STATE_LOCK:
        state = load_pairing()
        if user_id not in state["approved"]:
            raise ValueError("telegram pairing user is not approved")
        state["approved"] = [entry for entry in state["approved"] if entry != user_id]
        state["pending"] = {
            code: entry
            for code, entry in state["pending"].items()
            if entry["user_id"] != user_id
        }
        commit_pairing(state)
        return {"status": "revoked", "user_id": user_id}


def pairing_approved(user_id, now_ms=None):
    if isinstance(user_id, bool) or not isinstance(user_id, int):
        return False
    now_ms = int(time.time() * 1000) if now_ms is None else _require_int(now_ms, "telegram pairing time", 0)
    with _STATE_LOCK:
        state = load_pairing()
        if prune_pairing(state, now_ms):
            commit_pairing(state)
        return user_id in state["approved"]


def user_authorized(config, message):
    authz = config["authz"]
    if authz.get("allow_all_users", False) or env_flag(authz.get("allow_all_users_env", "")):
        return True
    user = (message or {}).get("from") or {}
    chat = (message or {}).get("chat") or {}
    user_id = user.get("id")
    allowed_users = list(authz.get("allowed_user_ids", [])) + env_int_list(
        authz.get("allowed_user_ids_env", "")
    )
    allowed_chats = list(authz.get("allowed_chat_ids", [])) + env_int_list(
        authz.get("allowed_chat_ids_env", "")
    )
    if user_id in allowed_users or chat.get("id") in allowed_chats:
        return True
    if chat.get("type") in ("group", "supergroup", "channel"):
        gating = config["gating"]
        group_users = list(gating["group_allowed_user_ids"]) + env_int_list(
            gating["group_allowed_user_ids_env"]
        )
        group_chats = list(gating["group_allowed_chat_ids"]) + env_int_list(
            gating["group_allowed_chat_ids_env"]
        )
        if user_id in group_users or chat.get("id") in group_chats:
            return True
        if gating.get("guest_mode") and _explicitly_mentions_this_bot(config, message):
            return True
    pairing = authz.get("pairing") or {}
    if pairing.get("enabled") and pairing_approved(user_id):
        return True
    return not authz.get("deny_unmatched", True)


def mention_targets(config):
    aliases = list(config["gating"].get("mention_aliases", []))
    username = (
        os.environ.get(config["gating"].get("bot_username_env", ""), "").strip()
        or _RESOLVED_BOT_USERNAME
        or ""
    )
    if username:
        aliases.append(f"@{username.lstrip('@')}")
    return [a.casefold() for a in aliases if a]


def _entity_mentions(message):
    text = (message or {}).get("text") or (message or {}).get("caption") or ""
    entities = (message or {}).get("entities") or (message or {}).get("caption_entities") or []
    mentions = []
    for entity in entities:
        if not isinstance(entity, dict) or entity.get("type") != "mention":
            continue
        fragment = _utf16_fragment(text, entity.get("offset"), entity.get("length"))
        if fragment and fragment.startswith("@"):
            mentions.append(fragment.casefold())
    return mentions


def _explicitly_mentions_this_bot(config, message):
    targets = set(mention_targets(config))
    if any(mention in targets for mention in _entity_mentions(message)):
        return True
    text = (message or {}).get("text") or (message or {}).get("caption") or ""
    return any(_contains_mention_token(text, target) for target in targets)


def _mentions_another_bot_exclusively(config, message):
    mentions = _entity_mentions(message)
    if not mentions:
        return False
    targets = set(mention_targets(config))
    bot_mentions = {
        mention
        for mention in mentions
        if mention.endswith("bot") or mention in targets
    }
    return bool(bot_mentions and bot_mentions.isdisjoint(targets))


def _configured_ids(config, key, env_key):
    return set(config["gating"].get(key, [])) | set(
        env_int_list(config["gating"].get(env_key, ""))
    )


def _topic_allowed(config, message):
    chat_id = ((message or {}).get("chat") or {}).get("id")
    thread_id = message.get("message_thread_id")
    coordinate = (chat_id, thread_id)
    ignored = {
        (item["chat_id"], item["thread_id"])
        for item in config["gating"].get("ignored_topics", [])
    }
    if coordinate in ignored:
        return False
    allowed = {
        (item["chat_id"], item["thread_id"])
        for item in config["gating"].get("allowed_topics", [])
    }
    return not allowed or coordinate in allowed


def _mention_pattern_matches(config, message):
    text = (message or {}).get("text") or (message or {}).get("caption") or ""
    return any(
        re.search(pattern, text, flags=re.IGNORECASE)
        for pattern in config["gating"].get("mention_patterns", [])
    )


def _utf16_fragment(text, offset, length):
    if isinstance(offset, bool) or not isinstance(offset, int) or offset < 0:
        return None
    if isinstance(length, bool) or not isinstance(length, int) or length <= 0:
        return None
    encoded = text.encode("utf-16-le")
    start = offset * 2
    end = (offset + length) * 2
    if end > len(encoded):
        return None
    try:
        return encoded[start:end].decode("utf-16-le")
    except UnicodeDecodeError:
        return None


def _contains_mention_token(text, target):
    escaped = re.escape(target)
    return re.search(rf"(?<![\w@]){escaped}(?!\w)", text, flags=re.IGNORECASE) is not None


def should_respond(config, message):
    chat = (message or {}).get("chat") or {}
    chat_type = chat.get("type")
    gating = config["gating"]
    if chat_type == "private":
        return gating.get("respond_in_private", True)
    if not _topic_allowed(config, message):
        return False
    if gating.get("exclusive_bot_mentions") and _mentions_another_bot_exclusively(
        config, message
    ):
        return False
    free_response = _configured_ids(
        config, "free_response_chat_ids", "free_response_chat_ids_env"
    )
    if chat.get("id") in free_response:
        return True
    if not gating.get("respond_in_groups_on_mention_only", True):
        return True
    text = (message or {}).get("text") or (message or {}).get("caption") or ""
    entities = (message or {}).get("entities") or (message or {}).get("caption_entities") or []
    targets = mention_targets(config)
    for entity in entities:
        if entity.get("type") == "mention":
            offset, length = entity.get("offset", 0), entity.get("length", 0)
            fragment = _utf16_fragment(text, offset, length)
            if fragment is not None and fragment.casefold() in targets:
                return True
    for target in targets:
        if _contains_mention_token(text, target):
            return True
    if _mention_pattern_matches(config, message):
        return True
    if gating.get("reply_to_bot_counts_as_mention", True):
        reply_from = ((message or {}).get("reply_to_message") or {}).get("from") or {}
        if reply_from.get("is_bot"):
            return True
    return False


def intake_disposition(config, message):
    if should_respond(config, message):
        chat_type = ((message or {}).get("chat") or {}).get("type")
        if chat_type in ("group", "supergroup", "channel"):
            gating = config["gating"]
            explicitly_addressed = (
                _explicitly_mentions_this_bot(config, message)
                or _mention_pattern_matches(config, message)
                or gating.get("reply_to_bot_counts_as_mention", True)
                and bool((((message or {}).get("reply_to_message") or {}).get("from") or {}).get("is_bot"))
            )
            free_response = _configured_ids(
                config, "free_response_chat_ids", "free_response_chat_ids_env"
            )
            open_group = not gating.get("respond_in_groups_on_mention_only", True)
            if not explicitly_addressed and not open_group and ((message or {}).get("chat") or {}).get("id") not in free_response:
                return "observe"
        return "respond"
    chat = (message or {}).get("chat") or {}
    if chat.get("type") not in ("group", "supergroup", "channel"):
        return None
    if config["gating"].get("exclusive_bot_mentions") and _mentions_another_bot_exclusively(
        config, message
    ):
        return None
    if not config["gating"].get("observe_unmentioned_group_messages"):
        return None
    observe_chats = _configured_ids(
        config, "observe_chat_ids", "observe_chat_ids_env"
    )
    if observe_chats and chat.get("id") not in observe_chats:
        return None
    if not _topic_allowed(config, message):
        return None
    return "observe"


MEDIA_INBOUND_FIELDS = (
    ("voice", "voice", ".ogg"),
    ("audio", "audio", ".mp3"),
    ("video", "video", ".mp4"),
    ("video_note", "video_note", ".mp4"),
    ("animation", "animation", ".mp4"),
    ("document", "document", ".bin"),
    ("sticker", "sticker", ".webp"),
)


def download_media_artifact(api, config, media_kind, blob, suffix):
    blob = _require_object(blob, f"telegram inbound {media_kind}")
    file_id = _require_string(blob.get("file_id"), f"telegram inbound {media_kind}.file_id")
    max_bytes = config["media"].get("max_download_bytes", 20971520)
    declared = blob.get("file_size")
    if declared is not None:
        declared = _require_int(declared, f"telegram inbound {media_kind}.file_size", 0)
    if declared is not None and declared > max_bytes:
        return {
            "kind": media_kind,
            "file_id": file_id,
            "size": declared,
            "skipped": "too_large",
        }
    file_meta = _require_object(api.call("getFile", {"file_id": file_id}), "telegram getFile result")
    file_path = _require_string(file_meta.get("file_path"), "telegram getFile result.file_path")
    try:
        payload = api.download_file(file_path, max_bytes)
    except TelegramMediaTooLarge:
        return {"kind": media_kind, "file_id": file_id, "skipped": "too_large"}
    file_name = blob.get("file_name")
    if file_name is not None:
        file_name = _require_string(file_name, f"telegram inbound {media_kind}.file_name")
        file_name = os.path.basename(file_name)
        extension = os.path.splitext(file_name)[1].lower()
        if extension and len(extension) <= 17:
            suffix = extension
    if media_kind == "sticker":
        if blob.get("is_animated") is True:
            suffix = ".tgs"
        elif blob.get("is_video") is True:
            suffix = ".webm"
    stamp = f"{time.time_ns()}"
    media_path = os.path.join(MEDIA_DIR, f"{media_kind}-{stamp}{suffix}")
    _atomic_write_bytes(media_path, payload)
    result = {
        "kind": media_kind,
        "payload_path": media_path,
        "file_unique_id": blob.get("file_unique_id"),
        "file_id": file_id,
        "telegram_file_path": file_path,
        "size": len(payload),
        "file_name": file_name,
        "mime_type": blob.get("mime_type"),
        "duration": blob.get("duration"),
        "width": blob.get("width"),
        "height": blob.get("height"),
    }
    preview_limit = config["media"]["max_text_preview_bytes"]
    if (
        media_kind == "document"
        and preview_limit > 0
        and suffix.casefold() in set(config["media"]["text_extensions"])
        and len(payload) <= preview_limit
    ):
        try:
            result["text_preview"] = payload.decode("utf-8")
        except UnicodeDecodeError:
            result["text_preview_error"] = "invalid_utf8"
    return result


def extract_inbound(api, config, message):
    parts = {
        "text": message.get("text") or message.get("caption") or "",
        "media": [],
        "location": None,
        "kind": "text",
    }
    allowed_media = set(config["media"]["inbound_kinds"])
    photos = message.get("photo")
    if photos:
        if not isinstance(photos, list) or not photos:
            raise ValueError("telegram inbound photo must be a non-empty array")
        for index, photo in enumerate(photos):
            photo = _require_object(photo, f"telegram inbound photo[{index}]")
            _require_string(photo.get("file_id"), f"telegram inbound photo[{index}].file_id")
            if photo.get("file_size") is not None:
                _require_int(photo["file_size"], f"telegram inbound photo[{index}].file_size", 0)
        if "photo" in allowed_media:
            largest = max(photos, key=lambda p: p.get("file_size") or 0)
            parts["media"].append(download_media_artifact(api, config, "photo", largest, ".jpg"))
            parts["kind"] = "photo"
        else:
            parts["media"].append({"kind": "photo", "skipped": "disabled_by_interface_config"})
    for field, kind, suffix in MEDIA_INBOUND_FIELDS:
        blob = message.get(field)
        if blob:
            if kind in allowed_media:
                parts["media"].append(download_media_artifact(api, config, kind, blob, suffix))
                parts["kind"] = kind
            else:
                parts["media"].append({
                    "kind": kind,
                    "file_id": blob.get("file_id"),
                    "skipped": "disabled_by_interface_config",
                })
    venue = message.get("venue")
    location = (venue or {}).get("location") if venue else message.get("location")
    if location:
        parts["location"] = {
            "latitude": location.get("latitude"),
            "longitude": location.get("longitude"),
            "title": (venue or {}).get("title"),
            "address": (venue or {}).get("address"),
        }
        parts["kind"] = "venue" if venue else "location"
    contact = message.get("contact")
    if contact:
        parts["contact"] = contact
        parts["kind"] = "contact"
    return parts


def emit_inbound_artifact(chat_id, thread_id, message, inbound, update_kind, update_id):
    payload = {
        "source": "telegram",
        "update_id": update_id,
        "update_kind": update_kind,
        "chat_id": chat_id,
        "thread_id": thread_id,
        "message_id": message.get("message_id"),
        "from": message.get("from"),
        "chat": message.get("chat"),
        "text": inbound["text"],
        "kind": inbound["kind"],
        "media": inbound["media"],
        "location": inbound.get("location"),
        "contact": inbound.get("contact"),
        "event": inbound.get("event"),
        "reply_to_message": enrich_reply_reference(
            chat_id, thread_id, message.get("reply_to_message")
        ),
        "disposition": inbound.get("disposition", "respond"),
    }
    body = json.dumps(payload, sort_keys=True).encode("utf-8")
    return hand_artifact(
        materialize_runtime_artifact(
            (
                "telegram.observed_message"
                if inbound.get("disposition") == "observe"
                else (
                    "telegram.inbound_batch"
                    if inbound.get("batch_count", 1) > 1
                    else "telegram.inbound_message"
                )
            ),
            body,
            f"telegram-in-{update_id}",
        )
    )


def process_binding_update(config, update):
    binding = config["binding"]
    chat_id = update.get("chat_id")
    thread_id = update.get("thread_id")
    if chat_id != binding["chat_id"] or thread_id != binding["thread_id"]:
        raise ValueError("telegram gateway delivered coordinates outside this interface binding")
    update_id = _require_int(update.get("update_id"), "telegram update_id", 0)
    message = _require_object(update.get("message"), "telegram binding message")
    inbound = _require_object(update.get("inbound"), "telegram binding inbound")
    update_kind = _require_string(update.get("update_kind"), "telegram binding update_kind")
    state = load_or_create_chat_state(chat_id, thread_id)
    previous_update = state.get("last_update_id")
    if previous_update is not None:
        _require_int(previous_update, "telegram chat state last_update_id", 0)
        if update_id <= previous_update:
            admitted_seq = state.get("last_update_admitted_seq")
            if isinstance(admitted_seq, bool) or not isinstance(admitted_seq, int):
                raise RuntimeError("telegram duplicate update has no durable admission evidence")
            return admitted_seq
    admitted_seq = emit_inbound_artifact(
        chat_id,
        thread_id,
        message,
        inbound,
        update_kind,
        update_id,
    )
    update_chat_state(
        chat_id,
        thread_id,
        last_user_message_id=message.get("message_id"),
        chat_type=(message.get("chat") or {}).get("type"),
        last_update_id=update_id,
        last_update_admitted_seq=admitted_seq,
    )
    return admitted_seq


def run_binding_intake(config):
    binding = config["binding"]
    ipc = config["driver_ipc"]
    backoff_ms = ipc["reconnect_backoff_ms"]
    while True:
        sock = None
        try:
            sock = socket.create_connection(
                (binding["host"], binding["port"]),
                ipc["connect_timeout_ms"] / 1000.0,
            )
            buffer = bytearray()
            _write_frame(
                sock,
                {
                    "kind": "register",
                    "interface": INTERFACE_ID,
                    "secret": _load_secret_handle(binding["secret_handle"]).hex(),
                },
                ipc["max_frame_bytes"],
            )
            registered = _read_frame(sock, buffer, ipc["max_frame_bytes"])
            if registered != {"kind": "registered", "interface": INTERFACE_ID}:
                raise RuntimeError("telegram gateway returned an invalid registration verdict")
            sock.settimeout(None)
            backoff_ms = ipc["reconnect_backoff_ms"]
            while True:
                frame = _read_frame(sock, buffer, ipc["max_frame_bytes"])
                if frame.get("kind") != "update" or not isinstance(frame.get("request_id"), int):
                    raise RuntimeError("telegram gateway sent an invalid update frame")
                request_id = frame["request_id"]
                try:
                    admitted_seq = process_binding_update(config, frame.get("update"))
                    verdict = {
                        "kind": "update_verdict",
                        "request_id": request_id,
                        "ok": True,
                        "admitted_seq": admitted_seq,
                    }
                except Exception as error:
                    verdict = {
                        "kind": "update_verdict",
                        "request_id": request_id,
                        "ok": False,
                        "error": sanitize_transport_error(error),
                    }
                _write_frame(sock, verdict, ipc["max_frame_bytes"])
        except Exception as error:
            sys.stderr.write(f"telegram binding IPC disconnected: {sanitize_transport_error(error)}\n")
            sys.stderr.flush()
            time.sleep(backoff_ms / 1000.0)
            backoff_ms = min(backoff_ms * 2, ipc["reconnect_backoff_cap_ms"])
        finally:
            if sock is not None:
                try:
                    sock.close()
                except OSError:
                    pass


def handle_callback_query(config, api, callback, update_id, hub):
    message = callback.get("message") or {}
    chat = message.get("chat") or {}
    chat_id = chat.get("id")
    thread_id = message.get("message_thread_id")
    from_user = callback.get("from") or {}
    probe = {"from": from_user, "chat": chat}
    if not user_authorized(config, probe):
        api.call("answerCallbackQuery", {"callback_query_id": callback["id"], "text": "Not authorized."})
        return
    api.call("answerCallbackQuery", {"callback_query_id": callback["id"]})
    synthetic = {
        "message_id": message.get("message_id"),
        "from": from_user,
        "chat": chat,
        "text": callback.get("data") or "",
    }
    inbound = {"text": callback.get("data") or "", "media": [], "kind": "callback"}
    binding = ensure_private_binding(config, hub, chat_id, thread_id)
    dispatch_gateway_update(
        binding,
        {
            "update_id": update_id,
            "update_kind": "callback_query",
            "chat_id": chat_id,
            "thread_id": thread_id,
            "message": synthetic,
            "inbound": inbound,
            "callback_query_id": callback.get("id"),
        },
    )


def handle_non_message_update(config, update, update_kind, hub):
    body = _require_object(update.get(update_kind), f"telegram {update_kind}")
    chat = body.get("chat") or {}
    if not isinstance(chat, dict) or not isinstance(chat.get("id"), int):
        return False
    actor = body.get("user") or body.get("from") or body.get("actor_chat") or {}
    if not isinstance(actor, dict):
        actor = {}
    message_id = body.get("message_id")
    if message_id is not None:
        _require_int(message_id, f"telegram {update_kind}.message_id", 1)
    thread_id = body.get("message_thread_id")
    validate_chat_coordinates(chat["id"], thread_id)
    synthetic = {
        "message_id": message_id,
        "from": actor,
        "chat": chat,
        "message_thread_id": thread_id,
        "text": "",
    }
    if not user_authorized(config, synthetic):
        return True
    if not _topic_allowed(config, synthetic):
        return True
    inbound = {
        "text": "",
        "media": [],
        "kind": update_kind,
        "disposition": "observe",
        "event": body,
    }
    binding = ensure_private_binding(config, hub, chat["id"], thread_id)
    dispatch_gateway_update(
        binding,
        {
            "update_id": update["update_id"],
            "update_kind": update_kind,
            "chat_id": chat["id"],
            "thread_id": thread_id,
            "message": synthetic,
            "inbound": inbound,
        },
    )
    return True


def handle_update(config, api, update, hub):
    update_id = update["update_id"]
    if "callback_query" in update:
        handle_callback_query(config, api, update["callback_query"], update_id, hub)
        return
    for event_kind in (
        "message_reaction",
        "message_reaction_count",
        "my_chat_member",
        "chat_member",
        "chat_join_request",
    ):
        if event_kind in update and handle_non_message_update(config, update, event_kind, hub):
            return
    update_kind = "message"
    message = update.get("message")
    if message is None and "edited_message" in update:
        message, update_kind = update["edited_message"], "edited_message"
    if message is None and "channel_post" in update:
        message, update_kind = update["channel_post"], "channel_post"
    if message is None and "edited_channel_post" in update:
        message, update_kind = update["edited_channel_post"], "edited_channel_post"
    if not message:
        return
    if not user_authorized(config, message):
        pairing = config["authz"].get("pairing") or {}
        if pairing.get("enabled"):
            emit_pairing_prompt(api, config, message)
        return
    disposition = intake_disposition(config, message)
    if disposition is None:
        return
    chat = message.get("chat") or {}
    chat_id = chat.get("id")
    thread_id = message.get("message_thread_id")
    validate_chat_coordinates(chat_id, thread_id)
    inbound = extract_inbound(api, config, message)
    inbound["disposition"] = disposition
    binding = ensure_private_binding(config, hub, chat_id, thread_id)
    if disposition == "respond":
        reactions = config["outbound"]["reactions"]
        if reactions.get("enabled"):
            set_reaction(
                config,
                api,
                chat_id,
                message.get("message_id"),
                reactions["in_progress"],
            )
        if config["outbound"]["typing"].get("enabled"):
            send_typing(config, api, chat_id, thread_id)
    dispatch_gateway_update(
        binding,
        {
            "update_id": update_id,
            "update_kind": update_kind,
            "chat_id": chat_id,
            "thread_id": thread_id,
            "message": message,
            "inbound": inbound,
        },
    )


def emit_pairing_prompt(api, config, message):
    chat = message.get("chat") or {}
    user = message.get("from") or {}
    if (chat.get("type")) != "private":
        return
    code = register_pending_pairing(user.get("id"))
    prompt = (config["authz"]["pairing"].get("prompt") or "Pairing code: {code}").format(code=code)
    api.call_flood_safe("sendMessage", {"chat_id": chat.get("id"), "text": prompt}, config)


def maybe_register_commands(config, api):
    commands = config["outbound"].get("commands") or {}
    if not commands.get("auto_register") or not commands.get("menu"):
        return
    bot_commands = [
        {"command": item["command"], "description": item.get("description", "")}
        for item in commands["menu"]
        if item.get("command")
    ]
    if not bot_commands:
        return
    for configured in commands["scopes"]:
        scope = {key: value for key, value in configured.items() if key != "language_code"}
        params = {"commands": bot_commands, "scope": scope}
        if configured.get("language_code"):
            params["language_code"] = configured["language_code"]
        api.call_flood_safe("setMyCommands", params, config)


def resolve_bot_identity(config, api):
    global _RESOLVED_BOT_USERNAME
    result = _require_object(api.call("getMe"), "telegram getMe result")
    if result.get("is_bot") is not True:
        raise RuntimeError("telegram getMe result does not identify a bot")
    username = _require_string(result.get("username"), "telegram getMe result.username")
    configured = os.environ.get(config["gating"]["bot_username_env"], "").strip().lstrip("@")
    if configured and configured.casefold() != username.casefold():
        raise RuntimeError(
            f"telegram configured bot username {configured!r} does not match getMe username {username!r}"
        )
    _RESOLVED_BOT_USERNAME = username


def register_status_indicator(config, api):
    status = config["outbound"]["status_indicator"]
    if not status["enabled"]:
        return
    api.call_flood_safe(
        "setMyShortDescription",
        {"short_description": status["online_text"]},
        config,
    )
    closed = {"value": False}

    def set_offline():
        if closed["value"]:
            return
        closed["value"] = True
        try:
            api.call_flood_safe(
                "setMyShortDescription",
                {"short_description": status["offline_text"]},
                config,
            )
        except Exception as error:
            sys.stderr.write(f"telegram offline status update failed: {api.sanitize_error(error)}\n")
            sys.stderr.flush()

    def stop(signum, _frame):
        set_offline()
        raise SystemExit(128 + signum)

    atexit.register(set_offline)
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, stop)


def process_update(config, api, update, hub):
    try:
        handle_update(config, api, update, hub)
    except (urllib.error.URLError, socket.timeout, TelegramError, RuntimeError):
        raise
    except Exception as error:
        update_id = update.get("update_id")
        raise RuntimeError(f"telegram update {update_id!r} was rejected: {error}") from error


def poll_updates(config, api, offset, hub):
    intake = config["intake"]
    params = {
        "timeout": intake["long_poll_timeout_s"],
        "allowed_updates": intake["allowed_updates"],
    }
    if offset:
        params["offset"] = offset
    elif intake.get("drop_pending_updates"):
        params["offset"] = -1
    updates = api.call("getUpdates", params, timeout_s=intake["long_poll_timeout_s"] + 5) or []
    for update in updates:
        process_update(config, api, update, hub)
        offset = update["update_id"] + 1
        commit_intake_offset(offset)
    return offset


def load_cursor():
    try:
        with open(CURSOR_PATH, "r", encoding="utf-8") as handle:
            state = json.load(handle)
    except FileNotFoundError:
        return 0
    state = _require_keys(state, {"schema_version", "next_seq"}, set(), "telegram ledger cursor")
    if state["schema_version"] != 1:
        raise ValueError("telegram ledger cursor schema_version must be 1")
    return _require_int(state["next_seq"], "telegram ledger cursor.next_seq", 0)


def commit_cursor(next_seq):
    _atomic_write_json(CURSOR_PATH, {"schema_version": 1, "next_seq": next_seq})


def blob_manifest_path(content_bytes):
    return os.path.join(BLOBS_DIR, content_hexdigest(content_bytes))


def content_hexdigest(content_bytes):
    if not isinstance(content_bytes, (bytes, bytearray, list)) or len(content_bytes) != 32:
        raise ValueError("telegram movement artifact content must be a 32-byte hash")
    if isinstance(content_bytes, list):
        if any(isinstance(item, bool) or not isinstance(item, int) or not 0 <= item <= 255 for item in content_bytes):
            raise ValueError("telegram movement artifact content contains a non-byte value")
        content_bytes = bytes(content_bytes)
    return bytes(content_bytes).hex()


def effect_path(source_seq):
    return os.path.join(EFFECTS_DIR, f"{source_seq}.json")


def load_effect(source_seq):
    try:
        with _open_private_read(effect_path(source_seq)) as handle:
            state = json.loads(handle.read().decode("utf-8"))
    except FileNotFoundError:
        return None
    sanitized = sanitize_transport_error(state)
    if sanitized != state:
        _atomic_write_json(effect_path(source_seq), sanitized, indent=2)
    state = sanitized
    _validate_effect(source_seq, state)
    return state


def commit_effect(source_seq, state):
    state = sanitize_transport_error(state)
    _validate_effect(source_seq, state)
    _atomic_write_json(effect_path(source_seq), state, indent=2)


def _validate_effect(source_seq, state):
    state = _require_keys(
        state,
        {
            "schema_version",
            "source_movement_seq",
            "source_artifact_hash",
            "run",
            "artifact_label",
            "status",
        },
        {"result", "error", "evidence_movement_seq"},
        f"telegram effect journal {source_seq}",
    )
    if state["schema_version"] != 1:
        raise ValueError(f"telegram effect journal {source_seq} schema_version must be 1")
    if _require_int(
        state["source_movement_seq"],
        f"telegram effect journal {source_seq}.source_movement_seq",
        0,
    ) != source_seq:
        raise ValueError(f"telegram effect journal {source_seq} has a mismatched source seq")
    if not isinstance(state["source_artifact_hash"], str) or not re.fullmatch(
        r"[0-9a-f]{64}", state["source_artifact_hash"]
    ):
        raise ValueError(f"telegram effect journal {source_seq} has a malformed artifact hash")
    _require_string(state["run"], f"telegram effect journal {source_seq}.run")
    _require_string(state["artifact_label"], f"telegram effect journal {source_seq}.artifact_label")
    status = state["status"]
    if status not in ("started", "succeeded", "indeterminate", "evidenced", "failure_evidenced"):
        raise ValueError(f"telegram effect journal {source_seq} has invalid status {status!r}")
    if "result" in state and not isinstance(state["result"], dict):
        raise ValueError(f"telegram effect journal {source_seq}.result must be an object")
    if "error" in state:
        _require_string(state["error"], f"telegram effect journal {source_seq}.error")
    if "evidence_movement_seq" in state:
        _require_int(
            state["evidence_movement_seq"],
            f"telegram effect journal {source_seq}.evidence_movement_seq",
            0,
        )
    if status in ("succeeded", "evidenced") and "result" not in state:
        raise ValueError(f"telegram effect journal {source_seq} status {status!r} requires result")
    if status == "indeterminate" and "error" not in state:
        raise ValueError(f"telegram effect journal {source_seq} indeterminate status requires error")
    if status in ("evidenced", "failure_evidenced") and "evidence_movement_seq" not in state:
        raise ValueError(f"telegram effect journal {source_seq} status {status!r} requires evidence seq")


def hand_delivery_result(source_seq, content_hash, run, result):
    payload = json.dumps(
        {
            "source_movement_seq": source_seq,
            "source_artifact_hash": content_hash,
            "run": run,
            "ok": True,
            "result": result,
        },
        sort_keys=True,
    ).encode("utf-8")
    return hand_artifact(
        materialize_runtime_artifact(
            "telegram.delivery.result",
            payload,
            f"telegram-delivery-result-{source_seq}",
        )
    )


def _read_bounded_regular(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode):
            raise ValueError(f"telegram {name} is not a regular file")
        if (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ValueError(f"telegram {name} changed while opening")
        if opened.st_size > maximum:
            raise ValueError(f"telegram {name} exceeds {maximum} bytes")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ValueError(f"telegram {name} grew above {maximum} bytes while reading")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def materialize_payload(content_bytes, manifest_maximum, payload_maximum):
    manifest_hash = content_hexdigest(content_bytes)
    manifest_bytes = _read_bounded_regular(blob_manifest_path(content_bytes), manifest_maximum, "cached artifact manifest")
    if octopus_content_hash(manifest_bytes) != manifest_hash:
        raise ValueError("telegram cached artifact manifest hash mismatch")
    manifest = json.loads(manifest_bytes)
    payload_content_hash = manifest.get("payload_content_hash")
    if (
        not isinstance(payload_content_hash, str)
        or len(payload_content_hash) != 64
        or any(char not in "0123456789abcdef" for char in payload_content_hash)
    ):
        raise ValueError("cached artifact manifest has no valid payload_content_hash")
    label = manifest.get("label")
    if not isinstance(label, str) or not label or label.strip() != label:
        raise ValueError("cached artifact manifest has no valid label")
    payload = _read_bounded_regular(os.path.join(BLOBS_DIR, payload_content_hash), payload_maximum, "cached artifact payload")
    if octopus_content_hash(payload) != payload_content_hash:
        raise ValueError("telegram cached artifact payload hash mismatch")
    return label, payload


def _movement_interface_endpoint(value):
    if not isinstance(value, dict) or set(value) != {"Interface"}:
        return None
    identifier = value.get("Interface")
    if not isinstance(identifier, str) or not identifier or identifier.strip() != identifier:
        return None
    return identifier


def _require_outbound_runtime(movement):
    metadata = _require_object(
        movement.get("invocation_metadata"),
        "telegram ledger movement.invocation_metadata",
    )
    runtime = _require_object(
        metadata.get("runtime"),
        "telegram ledger movement.invocation_metadata.runtime",
    )
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        _require_string(
            runtime.get(field),
            f"telegram ledger movement.invocation_metadata.runtime.{field}",
        )
    _require_int(
        runtime.get("epoch"),
        "telegram ledger movement.invocation_metadata.runtime.epoch",
        0,
    )
    if runtime["interface"] != INTERFACE_ID:
        raise ValueError("telegram outbound runtime interface does not match its exact target")
    return runtime


def _read_own_outbound_from_index(next_seq, limit):
    uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
    connection = sqlite3.connect(uri, uri=True, timeout=0.25)
    try:
        schema = connection.execute(
            "SELECT value FROM index_meta WHERE key = 'schema_version'"
        ).fetchone()
        if schema != ("3",):
            raise RuntimeError("telegram ledger index has an unsupported schema version")
        rows = connection.execute(
            "SELECT seq, artifact_content, runtime_run, runtime_checkpoint, from_id FROM movements "
            "WHERE runtime_interface = ? AND kind = 'InterfaceOutbound' "
            "AND from_kind = 'interface' AND to_kind = 'interface' AND to_id = ? AND seq >= ? "
            "ORDER BY seq ASC LIMIT ?",
            (INTERFACE_ID, INTERFACE_ID, next_seq, limit),
        ).fetchall()
    finally:
        connection.close()
    outbound = []
    for seq, artifact_content, run, checkpoint, source_interface in rows:
        seq = _require_int(seq, "telegram ledger index movement seq", 0)
        _require_string(source_interface, f"telegram ledger index movement {seq} from_id")
        content = None
        if artifact_content is not None:
            if not isinstance(artifact_content, str) or not re.fullmatch(r"[0-9a-f]{64}", artifact_content):
                raise RuntimeError(f"telegram ledger index movement {seq} has malformed artifact content")
            content = bytes.fromhex(artifact_content)
        _require_string(run, f"telegram ledger index movement {seq} runtime_run")
        _require_string(
            checkpoint,
            f"telegram ledger index movement {seq} runtime_checkpoint",
        )
        outbound.append((seq, content, run))
    return outbound


def _read_own_outbound_from_ledger(next_seq, limit, maximum_line_bytes):
    global _LEDGER_FALLBACK_OFFSET, _LEDGER_FALLBACK_SIZE, _LEDGER_FALLBACK_IDENTITY
    maximum_line_bytes = _require_int(
        maximum_line_bytes,
        "telegram ledger maximum line bytes",
        1,
    )
    with _LEDGER_LOCK:
        try:
            descriptor = os.open(MOVEMENTS_JSONL, os.O_RDONLY | os.O_NOFOLLOW)
        except FileNotFoundError:
            return []
        try:
            opened = os.fstat(descriptor)
            linked = os.lstat(MOVEMENTS_JSONL)
            identity = (opened.st_dev, opened.st_ino)
            if (
                not stat.S_ISREG(opened.st_mode)
                or stat.S_ISLNK(linked.st_mode)
                or not stat.S_ISREG(linked.st_mode)
                or identity != (linked.st_dev, linked.st_ino)
            ):
                raise RuntimeError("telegram movement ledger changed while opening")
            offset = _LEDGER_FALLBACK_OFFSET
            if (
                _LEDGER_FALLBACK_IDENTITY != identity
                or opened.st_size < _LEDGER_FALLBACK_SIZE
                or offset > opened.st_size
            ):
                offset = 0
            own = []
            with os.fdopen(descriptor, "rb") as handle:
                descriptor = -1
                handle.seek(offset)
                frontier_open = True
                while True:
                    line = handle.readline(maximum_line_bytes + 1)
                    if not line:
                        break
                    if len(line) > maximum_line_bytes:
                        raise RuntimeError("telegram ledger movement exceeds configured line bound")
                    if not line.endswith(b"\n"):
                        break
                    candidate_offset = handle.tell()
                    movement = _require_object(json.loads(line), "telegram ledger movement")
                    seq = _require_int(movement.get("seq"), "telegram ledger movement.seq", 0)
                    if seq < next_seq:
                        if frontier_open:
                            _LEDGER_FALLBACK_OFFSET = candidate_offset
                            _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
                            _LEDGER_FALLBACK_IDENTITY = identity
                        continue
                    if movement.get("kind") != "InterfaceOutbound":
                        if frontier_open:
                            _LEDGER_FALLBACK_OFFSET = candidate_offset
                            _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
                            _LEDGER_FALLBACK_IDENTITY = identity
                        continue
                    target_interface = _movement_interface_endpoint(movement.get("to"))
                    if target_interface != INTERFACE_ID:
                        if frontier_open:
                            _LEDGER_FALLBACK_OFFSET = candidate_offset
                            _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
                            _LEDGER_FALLBACK_IDENTITY = identity
                        continue
                    if _movement_interface_endpoint(movement.get("from")) is None:
                        if frontier_open:
                            _LEDGER_FALLBACK_OFFSET = candidate_offset
                            _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
                            _LEDGER_FALLBACK_IDENTITY = identity
                        continue
                    runtime = _require_outbound_runtime(movement)
                    artifact = movement.get("artifact")
                    frontier_open = False
                    own.append(
                        (
                            seq,
                            None if not artifact else artifact.get("content"),
                            runtime.get("run"),
                        )
                    )
                    if len(own) >= limit:
                        break
            return own
        finally:
            if descriptor >= 0:
                os.close(descriptor)


def read_own_outbound(next_seq, limit=128):
    global _LEDGER_INDEX_WARNING
    try:
        return _read_own_outbound_from_index(next_seq, limit)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
        message = str(error)
        if _LEDGER_INDEX_WARNING != message:
            sys.stderr.write(f"telegram ledger index unavailable; reading ledger truth: {message}\n")
            sys.stderr.flush()
            _LEDGER_INDEX_WARNING = message
        maximum_line_bytes = load_config()["driver_ipc"]["max_frame_bytes"]
        return _read_own_outbound_from_ledger(next_seq, limit, maximum_line_bytes)


def escape_markdown_v2(text):
    chunks = legacy_chunks(text, 1_000_000_000, pretty_tables=True)
    return "\n".join(rendered for _raw, rendered in chunks)


def telegram_text_units(text):
    return text_units(text)


def chunk_message(text, limit):
    return [raw for raw, _rendered in legacy_chunks(text, limit, pretty_tables=False)]


def render_message_chunks(text, parse_mode, limit, pretty_tables=True):
    if parse_mode != "MarkdownV2":
        return [(chunk, chunk) for chunk in chunk_message(text, limit)]
    return legacy_chunks(text, limit, pretty_tables=pretty_tables)


def set_reaction(config, api, chat_id, message_id, emoji):
    if message_id is None:
        return
    try:
        reaction = [] if emoji is None else [{"type": "emoji", "emoji": emoji}]
        api.call("setMessageReaction", {
            "chat_id": chat_id,
            "message_id": message_id,
            "reaction": reaction,
        })
    except (urllib.error.URLError, TelegramError) as error:
        sys.stderr.write(f"telegram setMessageReaction failed: {api.sanitize_error(error)}\n")
        sys.stderr.flush()


def send_typing(config, api, chat_id, thread_id, action=None):
    params = {"chat_id": chat_id, "action": action or config["outbound"]["typing"].get("action", "typing")}
    if thread_id is not None:
        params["message_thread_id"] = thread_id
    try:
        api.call("sendChatAction", params)
    except (urllib.error.URLError, TelegramError):
        pass


def _keyboard_markup(reply):
    keyboard = reply.get("inline_keyboard")
    if not keyboard:
        return None
    return {"inline_keyboard": keyboard}


def _thread_and_reply(params, thread_id, reply_to):
    if thread_id is not None:
        params["message_thread_id"] = thread_id
    if reply_to is not None:
        params["reply_parameters"] = {"message_id": reply_to}


MEDIA_OUT_METHODS = {
    "photo": ("sendPhoto", "photo", ".jpg"),
    "voice": ("sendVoice", "voice", ".ogg"),
    "video": ("sendVideo", "video", ".mp4"),
    "video_note": ("sendVideoNote", "video_note", ".mp4"),
    "audio": ("sendAudio", "audio", ".mp3"),
    "animation": ("sendAnimation", "animation", ".mp4"),
    "document": ("sendDocument", "document", ".bin"),
}


def _outbound_media_kinds(media):
    kinds = []
    for index, item in enumerate(media):
        if not isinstance(item, dict):
            raise ValueError("telegram outbound media entries must be objects")
        kind = _require_string(item.get("kind"), f"telegram outbound media[{index}].kind")
        if kind not in MEDIA_OUT_METHODS:
            raise ValueError(f"telegram outbound media[{index}] kind {kind!r} is unsupported")
        kinds.append(kind)
    return kinds


def _media_group_compatible(kinds):
    if len(kinds) < 2 or any(kind not in ("photo", "video", "audio", "document") for kind in kinds):
        return False
    distinct = set(kinds)
    if "audio" in distinct:
        return distinct == {"audio"}
    if "document" in distinct:
        return distinct == {"document"}
    return True


def _media_source(config, item, field, index):
    if not isinstance(item, dict):
        raise ValueError("telegram outbound media entries must be objects")
    allowed = {
        "kind",
        "telegram_file_id",
        "url",
        "blob_id",
        "artifact_ref",
        "payload_path",
        "filename",
        "content_type",
    }
    unknown = sorted(set(item) - allowed)
    if unknown:
        raise ValueError(f"telegram outbound media[{index}] has unknown keys: {', '.join(unknown)}")
    candidates = [
        key
        for key in ("telegram_file_id", "url", "blob_id", "artifact_ref", "payload_path")
        if item.get(key) is not None
    ]
    if len(candidates) != 1:
        raise ValueError("telegram outbound media requires exactly one declared source")
    source = candidates[0]
    if source == "payload_path":
        raise ValueError("telegram outbound media rejects ambient payload_path; use blob_id")
    if source == "artifact_ref":
        artifact_ref = item["artifact_ref"]
        if not isinstance(artifact_ref, dict) or set(artifact_ref) != {"blob_id"}:
            raise ValueError("telegram outbound artifact_ref must contain only blob_id")
        source = "blob_id"
        value = artifact_ref["blob_id"]
    else:
        value = item[source]
    value = _require_string(value, f"telegram outbound media[{index}].{source}")
    if source == "url":
        parsed = urllib.parse.urlparse(value)
        if parsed.scheme != "https" or not parsed.netloc:
            raise ValueError("telegram outbound media URL must be absolute HTTPS")
        return value, None
    if source == "telegram_file_id":
        if len(value) > 2048 or any(character.isspace() for character in value):
            raise ValueError("telegram outbound media file id is malformed")
        return value, None
    if len(value) != 64 or any(character not in "0123456789abcdef" for character in value):
        raise ValueError("telegram outbound media blob_id must be a lowercase content hash")
    path = os.path.join(BLOBS_DIR, value)
    maximum = config["media"]["max_upload_bytes"]
    payload = _read_bounded_regular(path, maximum, "outbound media blob")
    if octopus_content_hash(payload) != value:
        raise ValueError("telegram outbound media blob hash mismatch")
    filename = item.get("filename") or f"telegram-{index}{MEDIA_OUT_METHODS.get(item.get('kind'), MEDIA_OUT_METHODS['document'])[2]}"
    filename = _require_string(filename, f"telegram outbound media[{index}].filename")
    content_type = item.get("content_type") or mimetypes.guess_type(filename)[0] or "application/octet-stream"
    content_type = _require_string(content_type, f"telegram outbound media[{index}].content_type")
    if not re.fullmatch(r"[A-Za-z0-9!#$&^_.+-]+/[A-Za-z0-9!#$&^_.+-]+", content_type):
        raise ValueError(f"telegram outbound media[{index}].content_type is malformed")
    return f"attach://{field}", (filename, content_type, payload)


def _rich_permanent_failure(error):
    if not isinstance(error, TelegramError):
        return False
    description = error.description.casefold()
    return bool(
        error.error_code in (400, 404)
        or "unsupported" in description
        or "not implemented" in description
        or "method not found" in description
        or "no such method" in description
    )


def _rich_eligible(config, content, draft=False, explicit=None):
    global _RICH_SEND_DISABLED, _RICH_DRAFT_DISABLED
    rich = config["outbound"]["rich_messages"]
    if not rich["enabled"] or _RICH_SEND_DISABLED:
        return False
    if draft and (not rich["drafts_enabled"] or _RICH_DRAFT_DISABLED):
        return False
    if explicit is False:
        return False
    if not draft and explicit is not True and not needs_rich_rendering(content):
        return False
    return rich_safe(
        content,
        rich["max_chars"],
        reject_details_math=rich["reject_details_math"],
        reject_cjk=rich["reject_cjk"],
    )


def _rich_payload(config, content):
    rich = config["outbound"]["rich_messages"]
    return rich_message_payload(content, rich["skip_entity_detection"])


def _try_send_rich(config, api, chat_state, reply, content, reply_to, silent, markup):
    global _RICH_SEND_DISABLED
    if not _rich_eligible(config, content, explicit=reply.get("rich")):
        return None
    params = {
        "chat_id": chat_state["chat_id"],
        "rich_message": _rich_payload(config, content),
    }
    _thread_and_reply(params, chat_state.get("thread_id"), reply_to)
    if silent:
        params["disable_notification"] = True
    if config["outbound"].get("disable_link_previews"):
        params["link_preview_options"] = {"is_disabled": True}
    if markup:
        params["reply_markup"] = markup
    try:
        result = api.call_flood_safe("sendRichMessage", params, config)
    except TelegramError as error:
        if not _rich_permanent_failure(error):
            raise
        if error.error_code == 404 or "method" in error.description.casefold():
            _RICH_SEND_DISABLED = True
        return None
    if not isinstance(result, dict):
        raise RuntimeError("telegram sendRichMessage returned a non-object success result")
    message_id = result.get("message_id")
    if isinstance(message_id, bool) or not isinstance(message_id, int) or message_id <= 0:
        raise RuntimeError("telegram sendRichMessage returned no positive message_id")
    return message_id


def _try_edit_rich(config, api, chat_state, reply, content, edit_target):
    global _RICH_SEND_DISABLED
    if not _rich_eligible(config, content, explicit=reply.get("rich")):
        return False
    params = {
        "chat_id": chat_state["chat_id"],
        "message_id": edit_target,
        "rich_message": _rich_payload(config, content),
    }
    markup = _keyboard_markup(reply)
    if markup:
        params["reply_markup"] = markup
    if config["outbound"].get("disable_link_previews"):
        params["link_preview_options"] = {"is_disabled": True}
    try:
        api.call_flood_safe("editMessageText", params, config)
    except TelegramError as error:
        if "not modified" in error.description.casefold():
            return True
        if not _rich_permanent_failure(error):
            raise
        if error.error_code == 404 or "method" in error.description.casefold():
            _RICH_SEND_DISABLED = True
        return False
    return True


def _native_draft_allowed(config, chat_state):
    streaming = config["outbound"]["streaming"]
    if streaming["transport"] == "edit":
        return False
    if streaming["native_drafts_private_only"] and chat_state.get("chat_type") != "private":
        return False
    return True


def _try_native_draft(config, api, chat_state, reply, content):
    global _RICH_DRAFT_DISABLED
    if not _native_draft_allowed(config, chat_state):
        return False
    draft_id = chat_state.get("stream_draft_id")
    if isinstance(draft_id, bool) or not isinstance(draft_id, int) or draft_id <= 0:
        return False
    thread_id = chat_state.get("thread_id")
    if _rich_eligible(config, content, draft=True, explicit=reply.get("rich")):
        params = {
            "chat_id": chat_state["chat_id"],
            "draft_id": draft_id,
            "rich_message": _rich_payload(config, content),
        }
        if thread_id is not None:
            params["message_thread_id"] = thread_id
        try:
            return bool(api.call_flood_safe("sendRichMessageDraft", params, config))
        except TelegramError as error:
            if not _rich_permanent_failure(error):
                return False
            _RICH_DRAFT_DISABLED = True
    chunks = render_message_chunks(
        content,
        config["outbound"]["parse_mode"],
        config["outbound"]["max_message_len"],
        config["outbound"]["rich_messages"]["pretty_tables"],
    )
    if not chunks:
        return False
    raw, rendered = chunks[-1]
    params = {
        "chat_id": chat_state["chat_id"],
        "draft_id": draft_id,
        "text": rendered,
        "parse_mode": config["outbound"]["parse_mode"],
    }
    if thread_id is not None:
        params["message_thread_id"] = thread_id
    try:
        return bool(api.call_flood_safe("sendMessageDraft", params, config))
    except TelegramError as error:
        if error.error_code != 400:
            return False
        fallback = dict(params)
        fallback.pop("parse_mode", None)
        fallback["text"] = raw
        try:
            return bool(api.call_flood_safe("sendMessageDraft", fallback, config))
        except TelegramError:
            return False


def render_outbound(config, api, chat_state, reply):
    chat_id = chat_state["chat_id"]
    thread_id = chat_state.get("thread_id")
    outbound = config["outbound"]
    parse_mode = outbound["parse_mode"]
    limit = outbound["max_message_len"]
    pretty_tables = outbound["rich_messages"]["pretty_tables"]
    caption = reply.get("caption", "")
    caption_chunks = (
        render_message_chunks(
            caption,
            parse_mode,
            outbound["max_caption_len"],
            pretty_tables,
        )
        if caption
        else []
    )
    rendered_caption = caption_chunks[0][1] if caption_chunks else ""
    caption_overflow = "".join(raw for raw, _rendered in caption_chunks[1:])
    reply_to = reply.get("reply_to_message_id")
    silent = reply.get("silent", outbound.get("silent_default", False))
    markup = _keyboard_markup(reply)
    sent_message_id = None
    api_results = []

    for call in reply.get("api_calls") or []:
        call = _require_keys(
            call,
            {"method"},
            {"params"},
            "telegram api_calls entry",
        )
        method = _require_string(call.get("method"), "telegram api_calls method")
        if method not in set(outbound["allowed_api_methods"]):
            raise ValueError(f"telegram API method {method!r} is not declared by this interface")
        raw_params = call.get("params", {})
        if not isinstance(raw_params, dict):
            raise ValueError("telegram api_calls params must be an object")
        params = dict(raw_params)
        method_options = outbound["api_method_options"].get(
            method,
            {"inject_chat_id": True, "inject_thread_id": True},
        )
        if method_options["inject_chat_id"]:
            params["chat_id"] = chat_id
        if method_options["inject_thread_id"] and thread_id is not None:
            params["message_thread_id"] = thread_id
        result = api.call_flood_safe(method, params, config)
        api_results.append({"method": method, "result": result})
        if isinstance(result, dict):
            sent_message_id = result.get("message_id", sent_message_id)

    action = reply.get("chat_action")
    if action:
        send_typing(config, api, chat_id, thread_id, action=action)

    edit_target = reply.get("edit_message_id") or (
        chat_state.get("stream_message_id") if reply.get("stream") else None
    )
    if reply.get("stream") and not reply.get("final") and not edit_target and reply.get("text"):
        now_ms = time.time_ns() // 1_000_000
        if now_ms - chat_state.get("stream_last_edit_ms", 0) < outbound["min_edit_interval_ms"]:
            return {"message_id": None, "api_results": api_results}
        if _try_native_draft(config, api, chat_state, reply, reply["text"]):
            update_chat_state(chat_id, thread_id, stream_last_edit_ms=now_ms)
            return {"message_id": None, "api_results": api_results}
    if edit_target and reply.get("text"):
        return {
            "message_id": _render_stream_edit(config, api, chat_state, reply, edit_target),
            "api_results": api_results,
        }

    media = reply.get("media") or []
    media_kinds = _outbound_media_kinds(media)
    if _media_group_compatible(media_kinds):
        sent_message_id = _render_media_group(
            config,
            api,
            chat_id,
            thread_id,
            media,
            rendered_caption,
            reply_to,
            silent,
        )
        media = []
        media_kinds = []
    for media_index, item in enumerate(media):
        kind = media_kinds[media_index]
        method, field, _suffix = MEDIA_OUT_METHODS[kind]
        source, upload = _media_source(config, item, field, media_index)
        params = {"chat_id": chat_id, field: source}
        _thread_and_reply(params, thread_id, reply_to)
        if silent:
            params["disable_notification"] = True
        if rendered_caption and media_index == 0:
            params["caption"] = rendered_caption
            params["parse_mode"] = parse_mode
        if markup and media_index == len(media) - 1:
            params["reply_markup"] = markup
        if upload is None:
            result = api.call_flood_safe(method, params, config)
        else:
            result = api.call_multipart_flood_safe(method, params, {field: upload}, config)
        if isinstance(result, dict):
            sent_message_id = result.get("message_id", sent_message_id)
        reply_to = None

    text = reply.get("text", "")
    if caption_overflow:
        text = caption_overflow if not text else f"{caption_overflow}\n\n{text}"
    if text:
        rich_message_id = _try_send_rich(
            config,
            api,
            chat_state,
            reply,
            text,
            reply_to,
            silent,
            markup,
        )
        if rich_message_id is not None:
            sent_message_id = rich_message_id
        else:
            chunks = render_message_chunks(text, parse_mode, limit, pretty_tables)
            for index, (raw_chunk, rendered_chunk) in enumerate(chunks):
                params = {"chat_id": chat_id, "text": rendered_chunk, "parse_mode": parse_mode}
                _thread_and_reply(params, thread_id, reply_to)
                reply_to = None
                if silent:
                    params["disable_notification"] = True
                if outbound.get("disable_link_previews"):
                    params["link_preview_options"] = {"is_disabled": True}
                if markup and index == len(chunks) - 1:
                    params["reply_markup"] = markup
                result = _send_message_resilient(config, api, params, raw_chunk)
                if isinstance(result, dict):
                    sent_message_id = result.get("message_id", sent_message_id)

    return {"message_id": sent_message_id, "api_results": api_results}


def _render_stream_edit(config, api, chat_state, reply, edit_target):
    chat_id = chat_state["chat_id"]
    thread_id = chat_state.get("thread_id")
    outbound = config["outbound"]
    parse_mode = outbound["parse_mode"]
    min_interval_ms = outbound.get("min_edit_interval_ms", 1000)
    now_ms = time.time_ns() // 1_000_000
    last_ms = chat_state.get("stream_last_edit_ms", 0)
    if reply.get("stream") and not reply.get("final") and now_ms - last_ms < min_interval_ms:
        return edit_target
    text = reply.get("text", "")
    if reply.get("final") and _try_edit_rich(config, api, chat_state, reply, text, edit_target):
        update_chat_state(
            chat_id,
            thread_id,
            stream_last_edit_ms=now_ms,
        )
        return edit_target
    chunks = render_message_chunks(
        text,
        parse_mode,
        outbound["max_message_len"],
        outbound["rich_messages"]["pretty_tables"],
    )
    if not chunks:
        raise ValueError("telegram stream edit requires non-empty text")
    first_index = 0 if reply.get("final") else len(chunks) - 1
    raw, rendered = chunks[first_index]
    params = {
        "chat_id": chat_id,
        "message_id": edit_target,
        "text": rendered,
        "parse_mode": parse_mode,
    }
    markup = _keyboard_markup(reply)
    if markup and (not reply.get("final") or len(chunks) == 1):
        params["reply_markup"] = markup
    try:
        api.call_flood_safe("editMessageText", params, config)
    except TelegramError as error:
        if "not modified" in error.description.lower():
            pass
        elif params.get("parse_mode") and error.error_code == 400:
            fallback = dict(params)
            fallback.pop("parse_mode", None)
            fallback["text"] = raw
            api.call_flood_safe("editMessageText", fallback, config)
        else:
            raise
    last_message_id = edit_target
    if reply.get("final") and len(chunks) > 1:
        silent = reply.get("silent", outbound.get("silent_default", False))
        for index, (raw_chunk, rendered_chunk) in enumerate(chunks[1:], start=1):
            send_params = {
                "chat_id": chat_id,
                "text": rendered_chunk,
                "parse_mode": parse_mode,
            }
            if thread_id is not None:
                send_params["message_thread_id"] = thread_id
            if silent:
                send_params["disable_notification"] = True
            if outbound.get("disable_link_previews"):
                send_params["link_preview_options"] = {"is_disabled": True}
            if markup and index == len(chunks) - 1:
                send_params["reply_markup"] = markup
            result = _send_message_resilient(config, api, send_params, raw_chunk)
            if isinstance(result, dict):
                last_message_id = result.get("message_id", last_message_id)
    update_chat_state(
        chat_id,
        thread_id,
        stream_last_edit_ms=now_ms,
    )
    return last_message_id


def _render_media_group(config, api, chat_id, thread_id, media, rendered_caption, reply_to, silent):
    if len(media) > config["media"]["max_media_group_items"]:
        raise ValueError("telegram outbound media group exceeds max_media_group_items")
    kinds = _outbound_media_kinds(media)
    if not _media_group_compatible(kinds):
        raise ValueError("telegram outbound media group contains incompatible media kinds")
    group = []
    uploads = {}
    for index, item in enumerate(media):
        kind = kinds[index]
        upload_field = f"media_{index}"
        source, upload = _media_source(config, item, upload_field, index)
        entry = {"type": kind, "media": source}
        if upload is not None:
            uploads[upload_field] = upload
        if index == 0 and rendered_caption:
            entry["caption"] = rendered_caption
            entry["parse_mode"] = config["outbound"]["parse_mode"]
        group.append(entry)
    params = {"chat_id": chat_id, "media": group}
    _thread_and_reply(params, thread_id, reply_to)
    if silent:
        params["disable_notification"] = True
    if uploads:
        result = api.call_multipart_flood_safe("sendMediaGroup", params, uploads, config)
    else:
        result = api.call_flood_safe("sendMediaGroup", params, config)
    if isinstance(result, list) and result:
        return result[-1].get("message_id")
    return None


def _send_message_resilient(config, api, params, fallback_text):
    try:
        return api.call_flood_safe("sendMessage", params, config)
    except TelegramError as error:
        if params.get("parse_mode") and error.error_code == 400:
            fallback = dict(params)
            fallback.pop("parse_mode", None)
            fallback["text"] = fallback_text
            return api.call_flood_safe("sendMessage", fallback, config)
        raise


def _validate_reply_shape(reply):
    allowed = {
        "chat_id",
        "thread_id",
        "text",
        "caption",
        "chat_action",
        "stream",
        "final",
        "silent",
        "cancelled",
        "error",
        "reply_to_message_id",
        "edit_message_id",
        "media",
        "api_calls",
        "inline_keyboard",
        "rich",
    }
    unknown = sorted(set(reply) - allowed)
    if unknown:
        raise ValueError(f"telegram outbound payload has unknown keys: {', '.join(unknown)}")
    for field in ("text", "caption", "chat_action"):
        value = reply.get(field)
        if value is not None and not isinstance(value, str):
            raise ValueError(f"telegram outbound {field} must be a string")
    for field in ("stream", "final", "silent", "cancelled", "error", "rich"):
        value = reply.get(field)
        if value is not None and not isinstance(value, bool):
            raise ValueError(f"telegram outbound {field} must be a boolean")
    for field in ("reply_to_message_id", "edit_message_id"):
        value = reply.get(field)
        if value is not None and (
            isinstance(value, bool) or not isinstance(value, int) or value <= 0
        ):
            raise ValueError(f"telegram outbound {field} must be a positive integer")
    for field in ("media", "api_calls", "inline_keyboard"):
        value = reply.get(field)
        if value is not None and not isinstance(value, list):
            raise ValueError(f"telegram outbound {field} must be an array")
    if not any(
        reply.get(field)
        for field in ("text", "caption", "chat_action", "media", "api_calls", "cancelled", "error")
    ):
        raise ValueError("telegram outbound payload has no declared effect")
    if reply.get("caption") and not reply.get("media"):
        raise ValueError("telegram outbound caption requires media")


def render_reply(config, api, request_bytes, expected_binding=None):
    try:
        reply = json.loads(request_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"telegram outbound payload is not valid JSON: {error}") from error
    if not isinstance(reply, dict):
        raise ValueError("telegram outbound payload must be a JSON object")
    _validate_reply_shape(reply)
    if expected_binding is None:
        chat_id = reply.get("chat_id")
        thread_id = reply.get("thread_id")
    else:
        chat_id = expected_binding["chat_id"]
        thread_id = expected_binding["thread_id"]
        if "chat_id" in reply and reply["chat_id"] != chat_id:
            raise ValueError("telegram outbound chat_id escapes this interface binding")
        if "thread_id" in reply and reply["thread_id"] != thread_id:
            raise ValueError("telegram outbound thread_id escapes this interface binding")
        reply["chat_id"] = chat_id
        reply["thread_id"] = thread_id
    validate_chat_coordinates(chat_id, thread_id)
    chat_state = load_or_create_chat_state(chat_id, thread_id)
    outbound_result = render_outbound(config, api, chat_state, reply)
    sent_message_id = outbound_result["message_id"]
    reactions = config["outbound"]["reactions"]
    if reactions.get("enabled", False) and (not reply.get("stream") or reply.get("final")):
        last_user = chat_state.get("last_user_message_id")
        if reply.get("cancelled") and reactions.get("clear_on_cancel", True):
            set_reaction(config, api, chat_state["chat_id"], last_user, None)
        else:
            outcome_emoji = reactions["failure"] if reply.get("error") else reactions["success"]
            set_reaction(config, api, chat_state["chat_id"], last_user, outcome_emoji)
    updates = {}
    if sent_message_id is not None:
        updates["last_bot_message_id"] = sent_message_id
        if reply.get("stream") and not chat_state.get("stream_message_id"):
            updates["stream_message_id"] = sent_message_id
    if reply.get("final"):
        updates["stream_message_id"] = None
        updates["stream_draft_id"] = None
    if updates:
        update_chat_state(
            chat_state["chat_id"],
            chat_state.get("thread_id"),
            **updates,
        )
    if sent_message_id is not None:
        remember_sent_message(
            chat_state["chat_id"],
            chat_state.get("thread_id"),
            sent_message_id,
            reply.get("text") or reply.get("caption") or "",
        )
    return {
        "chat_id": chat_state["chat_id"],
        "thread_id": chat_state.get("thread_id"),
        "message_id": sent_message_id,
        "api_results": outbound_result["api_results"],
    }


class _WebhookHandler(http.server.BaseHTTPRequestHandler):
    server_version = "octopus-telegram/1"

    def do_POST(self):
        if urllib.parse.urlparse(self.path).path != self.server.webhook_path:
            self.send_response(404)
            self.end_headers()
            return
        expected = self.server.webhook_secret
        got = self.headers.get("X-Telegram-Bot-Api-Secret-Token", "")
        if not hmac.compare_digest(got, expected):
            self.send_response(403)
            self.end_headers()
            return
        if self.headers.get("Transfer-Encoding"):
            self.send_response(400)
            self.end_headers()
            return
        raw_length = self.headers.get("Content-Length")
        try:
            length = int(raw_length) if raw_length is not None else -1
        except ValueError:
            length = -1
        if length < 1 or length > self.server.max_body_bytes:
            self.send_response(413 if length > self.server.max_body_bytes else 400)
            self.end_headers()
            return
        body = self.rfile.read(length)
        if len(body) != length:
            self.send_response(400)
            self.end_headers()
            return
        try:
            update = _require_object(json.loads(body), "telegram webhook update")
            update_id = _require_int(update.get("update_id"), "telegram webhook update_id", 0)
            with self.server.intake_lock:
                if update_id >= self.server.intake_offset:
                    process_update(self.server.config, self.server.api, update, self.server.hub)
                    self.server.intake_offset = update_id + 1
                    commit_intake_offset(self.server.intake_offset)
        except (ValueError, urllib.error.URLError, socket.timeout, TelegramError, RuntimeError) as error:
            sys.stderr.write(f"telegram webhook error: {self.server.api.sanitize_error(error)}\n")
            sys.stderr.flush()
            self.send_response(503)
            self.end_headers()
            return
        self.send_response(200)
        self.end_headers()

    def log_message(self, *_args):
        return


def start_webhook_server(config, api, hub):
    webhook = config["intake"]["webhook"]
    if config["intake"]["mode"] != "webhook":
        return False
    url = os.environ.get(webhook.get("enabled_env", ""), "").strip()
    if not url:
        raise RuntimeError(
            f"{webhook['enabled_env']} is required when intake.mode is webhook"
        )
    parsed_url = urllib.parse.urlparse(url)
    if parsed_url.scheme != "https" or parsed_url.path != webhook["path"]:
        raise RuntimeError("telegram webhook URL must be HTTPS and match intake.webhook.path")
    secret = os.environ.get(webhook.get("secret_env", ""), "").strip()
    if not secret:
        raise RuntimeError(
            f"{webhook.get('secret_env')} is required when the Telegram webhook is enabled "
            f"(a missing secret makes the endpoint accept forged updates)"
        )
    api.call("setWebhook", {
        "url": url,
        "secret_token": secret,
        "allowed_updates": config["intake"]["allowed_updates"],
        "drop_pending_updates": config["intake"].get("drop_pending_updates", False),
    })
    port = webhook["port"]
    override = os.environ.get(webhook.get("port_env", ""), "").strip()
    if override.isdigit():
        port = int(override)
    server = http.server.HTTPServer((webhook["listen"], port), _WebhookHandler)
    server.config = config
    server.api = api
    server.hub = hub
    server.webhook_secret = secret
    server.webhook_path = webhook["path"]
    server.max_body_bytes = webhook["max_body_bytes"]
    server.intake_offset = load_intake_offset()
    server.intake_lock = threading.Lock()
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return True


def intake_once(config, api, intake_offset, hub, retry_state):
    intake = config["intake"]
    try:
        offset = poll_updates(config, api, intake_offset, hub)
        retry_state["conflicts"] = 0
        retry_state["network_backoff_ms"] = intake["network_backoff_ms"]
        return offset
    except TelegramError as error:
        if error.is_conflict:
            retry_state["conflicts"] += 1
            if retry_state["conflicts"] > intake["conflict_max_retries"]:
                raise RuntimeError(
                    f"telegram long polling remained in conflict for {retry_state['conflicts']} attempts"
                ) from error
            time.sleep(intake["conflict_backoff_ms"] / 1000.0)
            return intake_offset
        raise
    except (urllib.error.URLError, socket.timeout) as error:
        sys.stderr.write(f"telegram intake network error: {api.sanitize_error(error)}\n")
        sys.stderr.flush()
        backoff_ms = retry_state["network_backoff_ms"]
        time.sleep(backoff_ms / 1000.0)
        retry_state["network_backoff_ms"] = min(
            max(1, backoff_ms * 2),
            intake["network_backoff_cap_ms"],
        )
        return intake_offset


def _decode_output_payload(payload):
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"telegram interface output payload is not valid JSON: {error}") from error
    if not isinstance(value, dict):
        raise ValueError("telegram interface output payload must be an object")
    return value


def prepare_interface_output(config, label, payload):
    outbound = config["outbound"]
    body = _decode_output_payload(payload)
    binding = config["binding"]
    state = load_or_create_chat_state(binding["chat_id"], binding["thread_id"])
    if label in outbound["turn_start_labels"]:
        return (
            None,
            {"disposition": "turn_started", "label": label},
            {
                "stream_message_id": None,
                "stream_text": "",
                "stream_last_seq": None,
                "stream_last_edit_ms": 0,
                "stream_draft_id": int(time.time_ns() % 2147483646) + 1,
            },
        )
    if label in outbound["telemetry_labels"]:
        return None, {"disposition": "observed", "label": label}, {}
    if label in outbound["stream_labels"]:
        seq = _require_int(body.get("seq"), "telegram provider stream seq", 0)
        text = body.get("text")
        if not isinstance(text, str):
            raise ValueError("telegram provider stream output has no text string")
        previous = state.get("stream_last_seq")
        if previous is not None:
            _require_int(previous, "telegram stream_last_seq", 0)
            if seq <= previous:
                return None, {"disposition": "duplicate_stream_delta", "seq": seq}, {}
            if seq != previous + 1:
                raise ValueError(
                    f"telegram provider stream sequence jumped from {previous} to {seq}"
                )
        elif seq != 0:
            raise ValueError(f"telegram provider stream must begin at seq 0, got {seq}")
        accumulated = state.get("stream_text", "")
        if not isinstance(accumulated, str):
            raise ValueError("telegram stream_text state must be a string")
        accumulated += text
        return (
            {"text": accumulated, "stream": True, "final": False},
            {"disposition": "streamed", "seq": seq},
            {"stream_text": accumulated, "stream_last_seq": seq},
        )
    if label in outbound["reply_labels"]:
        if label == "telegram.outbound_message":
            return body, {"disposition": "delivered", "label": label}, {}
        text = body.get("text")
        if not isinstance(text, str) or not text:
            raise ValueError("telegram provider final output has no non-empty text")
        streamed = state.get("stream_message_id") is not None
        return (
            {"text": text, "stream": streamed, "final": True},
            {"disposition": "delivered", "label": label},
            {"stream_text": "", "stream_last_seq": None, "stream_draft_id": None},
        )
    if label in outbound["error_labels"]:
        detail = body.get("message") or body.get("error") or body.get("detail")
        if not isinstance(detail, str) or not detail:
            detail = json.dumps(body, ensure_ascii=False, sort_keys=True)
        streamed = state.get("stream_message_id") is not None
        return (
            {"text": f"Request failed: {detail}", "stream": streamed, "final": True, "error": True},
            {"disposition": "error_delivered", "label": label},
            {"stream_text": "", "stream_last_seq": None, "stream_draft_id": None},
        )
    raise ValueError(f"telegram interface does not declare outbound artifact label {label!r}")


def process_outbound(config, api, source_seq, content_bytes, run):
    if content_bytes is None or run is None:
        hand_failure(
            source_seq,
            "poisoned_outbound",
            {"error": "movement carries no artifact content hash or runtime run"},
            f"telegram-outbound-poison-{source_seq}",
        )
        return

    if config["mode"] != "binding":
        raise RuntimeError("telegram outbound delivery is only valid for a private binding interface")
    binding = config["binding"]
    if run != binding["run"]:
        hand_failure(
            source_seq,
            "boundary_rejected",
            {
                "error": "outbound movement run does not match this private interface",
                "expected_run": binding["run"],
                "observed_run": run,
            },
            f"telegram-outbound-run-failure-{source_seq}",
        )
        return

    content_hash = content_hexdigest(content_bytes)
    effect = load_effect(source_seq)
    if effect is not None:
        if not isinstance(effect, dict):
            raise RuntimeError(f"telegram effect journal {source_seq} is not an object")
        if effect.get("run") != run:
            raise RuntimeError(f"telegram effect journal {source_seq} has a mismatched run")
    if effect is not None and effect.get("source_artifact_hash") != content_hash:
        hand_failure(
            source_seq,
            "effect_journal_mismatch",
            {
                "expected": effect.get("source_artifact_hash"),
                "observed": content_hash,
            },
            f"telegram-effect-mismatch-{source_seq}",
        )
        return

    if effect is not None and effect.get("status") in ("evidenced", "failure_evidenced"):
        return

    if effect is not None and effect.get("status") == "succeeded":
        admitted_seq = hand_delivery_result(
            source_seq,
            content_hash,
            run,
            effect.get("result"),
        )
        effect["status"] = "evidenced"
        effect["evidence_movement_seq"] = admitted_seq
        commit_effect(source_seq, effect)
        return

    if effect is not None and effect.get("status") in ("started", "indeterminate"):
        admitted_seq = hand_failure(
            source_seq,
            "indeterminate_external_effect",
            {
                "source_artifact_hash": content_hash,
                "error": effect.get("error"),
            },
            f"telegram-effect-indeterminate-{source_seq}",
        )
        effect["status"] = "failure_evidenced"
        effect["evidence_movement_seq"] = admitted_seq
        commit_effect(source_seq, effect)
        return

    try:
        label, request_bytes = materialize_payload(
            content_bytes,
            config["driver_ipc"]["max_manifest_bytes"],
            config["driver_ipc"]["max_frame_bytes"],
        )
        reply, result, state_updates = prepare_interface_output(config, label, request_bytes)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        hand_failure(
            source_seq,
            "boundary_rejected",
            {"source_artifact_hash": content_hash, "error": api.sanitize_error(error)},
            f"telegram-outbound-boundary-failure-{source_seq}",
        )
        return

    effect = {
        "schema_version": 1,
        "source_movement_seq": source_seq,
        "source_artifact_hash": content_hash,
        "run": run,
        "artifact_label": label,
        "status": "started" if reply is not None else "succeeded",
    }
    if reply is None:
        if state_updates:
            update_chat_state(binding["chat_id"], binding["thread_id"], **state_updates)
        effect["result"] = result
        commit_effect(source_seq, effect)
        admitted_seq = hand_delivery_result(source_seq, content_hash, run, result)
        effect["status"] = "evidenced"
        effect["evidence_movement_seq"] = admitted_seq
        commit_effect(source_seq, effect)
        return

    commit_effect(source_seq, effect)
    try:
        delivery = render_reply(
            config,
            api,
            json.dumps(reply, ensure_ascii=False, separators=(",", ":")).encode("utf-8"),
            expected_binding=binding,
        )
        if state_updates:
            update_chat_state(binding["chat_id"], binding["thread_id"], **state_updates)
        result = {**result, "delivery": delivery}
    except Exception as error:
        safe_error = api.sanitize_error(error)
        effect["status"] = "indeterminate"
        effect["error"] = safe_error
        commit_effect(source_seq, effect)
        admitted_seq = hand_failure(
            source_seq,
            "indeterminate_external_effect",
            {"source_artifact_hash": content_hash, "error": safe_error},
            f"telegram-effect-indeterminate-{source_seq}",
        )
        effect["status"] = "failure_evidenced"
        effect["evidence_movement_seq"] = admitted_seq
        commit_effect(source_seq, effect)
        return

    effect["status"] = "succeeded"
    effect["result"] = result
    commit_effect(source_seq, effect)
    admitted_seq = hand_delivery_result(source_seq, content_hash, run, result)
    effect["status"] = "evidenced"
    effect["evidence_movement_seq"] = admitted_seq
    commit_effect(source_seq, effect)


def run_binding(config, api, base_url):
    binding = config["binding"]
    threading.Thread(target=run_binding_intake, args=(config,), daemon=True).start()
    next_seq = load_cursor()
    sys.stderr.write(
        f"telegram binding up: interface={INTERFACE_ID} run={binding['run']} "
        f"base={base_url} ledger_next_seq={next_seq}\n"
    )
    sys.stderr.flush()
    signal_ready()
    while True:
        own = read_own_outbound(next_seq)
        for source_seq, content_bytes, run in own:
            process_outbound(config, api, source_seq, content_bytes, run)
            next_seq = source_seq + 1
            commit_cursor(next_seq)
        time.sleep(0.05)


def run_gateway(config, api, base_url):
    global _INTAKE_BATCHER
    hub = BindingHub(config)
    _INTAKE_BATCHER = IntakeBatcher(config, hub)
    resolve_bot_identity(config, api)
    register_status_indicator(config, api)
    maybe_register_commands(config, api)
    _reconcile_known_bindings(config, hub)
    for proactive in config["proactive_bindings"]:
        ensure_private_binding(
            config,
            hub,
            proactive["chat_id"],
            proactive["thread_id"],
        )
    webhook_active = start_webhook_server(config, api, hub)
    intake_offset = load_intake_offset()
    retry_state = {
        "conflicts": 0,
        "network_backoff_ms": config["intake"]["network_backoff_ms"],
    }
    sys.stderr.write(
        f"telegram gateway up: interface={INTERFACE_ID} base={base_url} "
        f"webhook={webhook_active} intake_offset={intake_offset} "
        f"driver_ipc={hub.host}:{hub.port}\n"
    )
    sys.stderr.flush()
    signal_ready()
    while True:
        if webhook_active:
            time.sleep(1.0)
        else:
            intake_offset = intake_once(config, api, intake_offset, hub, retry_state)


def main():
    config = load_config()
    if "--pairing-approve" in sys.argv:
        index = sys.argv.index("--pairing-approve")
        if index + 1 >= len(sys.argv):
            raise ValueError("--pairing-approve requires a code")
        print(json.dumps(approve_pairing(sys.argv[index + 1]), sort_keys=True))
        return
    if "--pairing-revoke" in sys.argv:
        index = sys.argv.index("--pairing-revoke")
        if index + 1 >= len(sys.argv):
            raise ValueError("--pairing-revoke requires a user id")
        print(json.dumps(revoke_pairing(int(sys.argv[index + 1])), sort_keys=True))
        return
    start_runtime_verdict_reader()
    base_url = resolve_base_url(config)
    token = resolve_token(config)
    api = BotApi(base_url, token, config)
    if config["mode"] == "binding":
        run_binding(config, api, base_url)
    else:
        run_gateway(config, api, base_url)


if __name__ == "__main__":
    main()
