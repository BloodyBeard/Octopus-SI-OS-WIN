import base64
import hashlib
import html
import json
import os
import re
import tempfile
import time
from dataclasses import dataclass
from email import policy
from email.header import decode_header
from email.message import EmailMessage, Message
from email.parser import BytesParser
from email.utils import format_datetime, getaddresses, make_msgid, parsedate_to_datetime
from html.parser import HTMLParser
from pathlib import Path
from typing import Any, Callable, Iterable


_ADDRESS_RE = re.compile(r"^[^\s<>@,;:]+@[^\s<>@,;:]+$")
_MESSAGE_ID_RE = re.compile(r"^<[^<>\r\n]+>$")
_SAFE_FLAG_RE = re.compile(r"^(?:\\[A-Za-z]+|\$?[A-Za-z0-9_.-]{1,128})$")
_SAFE_LABEL_RE = re.compile(r"^[^\x00-\x1f\x7f]{1,255}$")
_HEX64_RE = re.compile(r"^[0-9a-f]{64}$")
_REGISTRY_ID_RE = re.compile(r"^[A-Za-z0-9_-]{1,128}$")
SUPPORTED_OUTBOUND_ACTIONS = frozenset(
    {
        "capabilities.get",
        "health.get",
        "folder.list",
        "folder.status",
        "folder.create",
        "folder.rename",
        "folder.delete",
        "folder.subscribe",
        "folder.unsubscribe",
        "message.list",
        "message.search",
        "message.fetch",
        "message.thread",
        "message.raw",
        "message.send",
        "message.reply",
        "message.reply_all",
        "message.forward",
        "message.copy",
        "message.move",
        "message.delete",
        "message.flags.add",
        "message.flags.remove",
        "message.labels.add",
        "message.labels.remove",
        "draft.create",
        "draft.update",
        "draft.delete",
        "draft.send",
        "attachment.fetch",
        "authorization.list",
        "authorization.request",
    }
)
SUPPORTED_AUTHORIZATION_CONTROL_ACTIONS = frozenset({"authorization.apply"})


class ContractError(ValueError):
    pass


def worker_role_artifact_bindings(worker: Any) -> list[dict[str, str]]:
    worker = require_string(worker, "email exact worker interface")
    if not _REGISTRY_ID_RE.fullmatch(worker):
        raise ContractError("email exact worker interface must be one registry identifier")
    policy_path = Path(__file__).resolve().parent.parent / worker / "policy.json"
    if policy_path.is_symlink() or not policy_path.is_file():
        raise ContractError(f"email exact worker policy {worker!r} must be a plain file")
    try:
        if policy_path.stat().st_size > 1024 * 1024:
            raise ContractError(f"email exact worker policy {worker!r} is oversized")
        policy_document = json.loads(policy_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ContractError(f"email exact worker policy {worker!r} is unreadable: {error}") from error
    policy_document = require_keys(
        policy_document,
        {"id", "ceiling", "role_artifacts"},
        set(),
        f"email exact worker policy {worker}",
    )
    if policy_document["id"] != worker:
        raise ContractError("email exact worker policy identity does not match its registry folder")
    roles = require_string_list(
        policy_document["role_artifacts"],
        f"email exact worker policy {worker}.role_artifacts",
        True,
    )
    return [{"id": artifact} for artifact in dict.fromkeys(roles)]


class HtmlTextExtractor(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.parts: list[str] = []
        self.hidden = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        lowered = tag.lower()
        if lowered in {"script", "style", "head"}:
            self.hidden += 1
        if self.hidden == 0 and lowered in {
            "br",
            "p",
            "div",
            "li",
            "tr",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
        }:
            self.parts.append("\n")
        if self.hidden == 0 and lowered == "li":
            self.parts.append("- ")

    def handle_endtag(self, tag: str) -> None:
        lowered = tag.lower()
        if lowered in {"script", "style", "head"} and self.hidden:
            self.hidden -= 1
        if self.hidden == 0 and lowered in {"p", "div", "li", "tr", "table", "blockquote"}:
            self.parts.append("\n")

    def handle_data(self, data: str) -> None:
        if self.hidden == 0:
            self.parts.append(data)

    def text(self) -> str:
        value = "".join(self.parts).replace("\r\n", "\n").replace("\r", "\n")
        value = re.sub(r"[ \t]+\n", "\n", value)
        value = re.sub(r"\n[ \t]+", "\n", value)
        value = re.sub(r"[ \t]{2,}", " ", value)
        value = re.sub(r"\n{3,}", "\n\n", value)
        return value.strip()


@dataclass(frozen=True)
class ResolvedAttachment:
    data: bytes
    filename: str
    media_type: str
    disposition: str
    content_id: str | None
    source: dict[str, Any]


def require_object(value: Any, name: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ContractError(f"{name} must be a JSON object")
    if any(not isinstance(key, str) for key in value):
        raise ContractError(f"{name} keys must be strings")
    return value


def require_keys(
    value: Any,
    required: set[str],
    optional: set[str],
    name: str,
) -> dict[str, Any]:
    obj = require_object(value, name)
    missing = sorted(required - set(obj))
    unknown = sorted(set(obj) - required - optional)
    if missing:
        raise ContractError(f"{name} is missing keys: {', '.join(missing)}")
    if unknown:
        raise ContractError(f"{name} has unknown keys: {', '.join(unknown)}")
    return obj


def require_string(value: Any, name: str, allow_empty: bool = False) -> str:
    if not isinstance(value, str) or value.strip() != value or (not value and not allow_empty):
        raise ContractError(f"{name} must be a{' possibly empty' if allow_empty else ' non-empty'} trimmed string")
    if "\x00" in value:
        raise ContractError(f"{name} must not contain NUL")
    return value


def require_int(value: Any, name: str, minimum: int | None = None, maximum: int | None = None) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ContractError(f"{name} must be an integer")
    if minimum is not None and value < minimum:
        raise ContractError(f"{name} must be at least {minimum}")
    if maximum is not None and value > maximum:
        raise ContractError(f"{name} must be at most {maximum}")
    return value


def require_bool(value: Any, name: str) -> bool:
    if not isinstance(value, bool):
        raise ContractError(f"{name} must be a boolean")
    return value


def require_text(value: Any, name: str) -> str:
    if not isinstance(value, str) or "\x00" in value:
        raise ContractError(f"{name} must be a string without NUL")
    return value


def require_string_list(value: Any, name: str, allow_empty: bool = False) -> list[str]:
    if not isinstance(value, list) or (not allow_empty and not value):
        raise ContractError(f"{name} must be a{' possibly empty' if allow_empty else ' non-empty'} array")
    result: list[str] = []
    seen: set[str] = set()
    for index, item in enumerate(value):
        text = require_string(item, f"{name}[{index}]")
        if text in seen:
            raise ContractError(f"{name} repeats {text!r}")
        seen.add(text)
        result.append(text)
    return result


def require_interface_binding(value: Any, name: str) -> str:
    binding = require_keys(value, {"interface", "config"}, set(), name)
    interface = require_string(binding["interface"], f"{name}.interface")
    config = require_keys(binding["config"], {"artifact_policy"}, set(), f"{name}.config")
    policy = require_object(config["artifact_policy"], f"{name}.config.artifact_policy")
    mode = require_string(policy.get("mode"), f"{name}.config.artifact_policy.mode")
    if mode == "all":
        require_keys(policy, {"mode"}, set(), f"{name}.config.artifact_policy")
    elif mode == "allow":
        require_keys(
            policy,
            {"mode", "names", "namespaces"},
            set(),
            f"{name}.config.artifact_policy",
        )
        names = require_string_list(policy["names"], f"{name}.config.artifact_policy.names", True)
        namespaces = require_string_list(
            policy["namespaces"],
            f"{name}.config.artifact_policy.namespaces",
            True,
        )
        if not names and not namespaces:
            raise ContractError(f"{name}.config.artifact_policy allow mode grants nothing")
        if any(namespace.endswith(".") for namespace in namespaces):
            raise ContractError(f"{name}.config.artifact_policy namespace must not end with '.'")
    else:
        raise ContractError(f"{name}.config.artifact_policy.mode must be 'all' or 'allow'")
    return interface


def _validate_env_name(value: Any, name: str) -> str:
    text = require_string(value, name)
    if not re.fullmatch(r"[A-Z][A-Z0-9_]*", text):
        raise ContractError(f"{name} must be an uppercase environment variable name")
    return text


def validate_config(config: Any) -> dict[str, Any]:
    cfg = require_keys(
        config,
        {"schema_version", "account", "intake", "authz", "authorization_control", "scopes", "content", "composition", "outbound", "driver_ipc", "gateway_run_template"},
        set(),
        "email config",
    )
    if cfg["schema_version"] != 1:
        raise ContractError("email config schema_version must be 1")
    account = require_keys(
        cfg["account"],
        {"id", "address_env", "display_name", "auth", "imap", "smtp", "mailboxes"},
        set(),
        "email account",
    )
    require_string(account["id"], "email account.id")
    _validate_env_name(account["address_env"], "email account.address_env")
    require_string(account["display_name"], "email account.display_name", True)
    auth = require_keys(
        account["auth"],
        {"mode", "password_env", "oauth_access_token_env"},
        set(),
        "email account.auth",
    )
    if auth["mode"] not in {"password", "oauth2"}:
        raise ContractError("email account.auth.mode must be password or oauth2")
    _validate_env_name(auth["password_env"], "email account.auth.password_env")
    _validate_env_name(auth["oauth_access_token_env"], "email account.auth.oauth_access_token_env")
    for transport_name in ("imap", "smtp"):
        required = {"host_env", "port_env", "port_default", "tls", "allow_plaintext", "timeout_ms", "connect_attempt_timeout_ms", "address_family"}
        if transport_name == "imap":
            required.add("send_client_id")
        else:
            required.add("save_sent")
        transport = require_keys(
            account[transport_name],
            required,
            set(),
            f"email account.{transport_name}",
        )
        _validate_env_name(transport["host_env"], f"email account.{transport_name}.host_env")
        _validate_env_name(transport["port_env"], f"email account.{transport_name}.port_env")
        require_int(transport["port_default"], f"email account.{transport_name}.port_default", 1, 65535)
        if transport["tls"] not in {"implicit", "starttls", "plain"}:
            raise ContractError(f"email account.{transport_name}.tls is invalid")
        require_bool(transport["allow_plaintext"], f"email account.{transport_name}.allow_plaintext")
        if transport["tls"] == "plain" and not transport["allow_plaintext"]:
            raise ContractError(f"email account.{transport_name} plaintext transport requires explicit allow_plaintext")
        require_int(transport["timeout_ms"], f"email account.{transport_name}.timeout_ms", 1000, 600000)
        require_int(transport["connect_attempt_timeout_ms"], f"email account.{transport_name}.connect_attempt_timeout_ms", 100, transport["timeout_ms"])
        if transport["address_family"] not in {"auto", "ipv4", "ipv6"}:
            raise ContractError(f"email account.{transport_name}.address_family is invalid")
        require_bool(transport["send_client_id" if transport_name == "imap" else "save_sent"], f"email account.{transport_name} boolean")
    mailboxes = require_keys(
        account["mailboxes"],
        {"inbox", "sent", "drafts", "trash", "archive"},
        set(),
        "email account.mailboxes",
    )
    for key, value in mailboxes.items():
        require_string(value, f"email account.mailboxes.{key}")
    intake = require_keys(
        cfg["intake"],
        {
            "enabled",
            "mailboxes",
            "poll_interval_ms",
            "idle_enabled",
            "idle_timeout_ms",
            "startup",
            "max_messages_per_poll",
            "dedup_retention",
            "pending_retention",
            "mark_seen_after_admission",
            "reconnect_backoff_ms",
            "reconnect_backoff_cap_ms",
            "emit_health_transitions",
        },
        set(),
        "email intake",
    )
    require_bool(intake["enabled"], "email intake.enabled")
    mailbox_aliases = require_string_list(intake["mailboxes"], "email intake.mailboxes")
    for alias in mailbox_aliases:
        if alias not in mailboxes:
            raise ContractError(f"email intake.mailboxes references unknown alias {alias!r}")
    resolved_intake_mailboxes = [mailboxes[alias].lower() for alias in mailbox_aliases]
    if len(set(resolved_intake_mailboxes)) != len(resolved_intake_mailboxes):
        raise ContractError("email intake.mailboxes resolves more than one alias to the same mailbox")
    require_int(intake["poll_interval_ms"], "email intake.poll_interval_ms", 250, 3600000)
    require_bool(intake["idle_enabled"], "email intake.idle_enabled")
    require_int(intake["idle_timeout_ms"], "email intake.idle_timeout_ms", 1000, 1740000)
    if intake["startup"] not in {"catch_up", "skip_existing"}:
        raise ContractError("email intake.startup must be catch_up or skip_existing")
    require_int(intake["max_messages_per_poll"], "email intake.max_messages_per_poll", 1, 1000)
    require_int(intake["dedup_retention"], "email intake.dedup_retention", 128, 1000000)
    require_int(intake["pending_retention"], "email intake.pending_retention", 16, 100000)
    require_bool(intake["mark_seen_after_admission"], "email intake.mark_seen_after_admission")
    backoff = require_int(intake["reconnect_backoff_ms"], "email intake.reconnect_backoff_ms", 100, 3600000)
    backoff_cap = require_int(intake["reconnect_backoff_cap_ms"], "email intake.reconnect_backoff_cap_ms", backoff, 86400000)
    if backoff_cap < backoff:
        raise ContractError("email intake reconnect cap must not be below the initial backoff")
    require_bool(intake["emit_health_transitions"], "email intake.emit_health_transitions")
    authz = require_keys(
        cfg["authz"],
        {
            "allow_all_env",
            "allowed_users_env",
            "allowed_domains_env",
            "deny_unmatched",
            "reject_self_messages",
            "automated_sender_fragments",
            "automated_headers",
            "deny_list_unsubscribe",
            "pairing",
        },
        set(),
        "email authz",
    )
    for key in ("allow_all_env", "allowed_users_env", "allowed_domains_env"):
        _validate_env_name(authz[key], f"email authz.{key}")
    require_bool(authz["deny_unmatched"], "email authz.deny_unmatched")
    require_bool(authz["reject_self_messages"], "email authz.reject_self_messages")
    require_string_list(authz["automated_sender_fragments"], "email authz.automated_sender_fragments", True)
    headers = require_object(authz["automated_headers"], "email authz.automated_headers")
    for key, values in headers.items():
        require_string(key, "email authz automated header name")
        require_string_list(values, f"email authz.automated_headers.{key}")
    require_bool(authz["deny_list_unsubscribe"], "email authz.deny_list_unsubscribe")
    pairing = require_keys(
        authz["pairing"],
        {"enabled", "code_ttl_ms", "max_pending", "subject", "body_template"},
        set(),
        "email authz.pairing",
    )
    require_bool(pairing["enabled"], "email authz.pairing.enabled")
    require_int(pairing["code_ttl_ms"], "email authz.pairing.code_ttl_ms", 60000, 604800000)
    require_int(pairing["max_pending"], "email authz.pairing.max_pending", 1, 10000)
    require_string(pairing["subject"], "email authz.pairing.subject")
    template = require_string(pairing["body_template"], "email authz.pairing.body_template")
    if "{code}" not in template:
        raise ContractError("email pairing body_template must contain {code}")
    authorization_control = require_keys(
        cfg["authorization_control"],
        {
            "source_interface",
            "outbound_interface",
            "request_checkpoint",
            "checkpoint",
            "request_artifact_label",
            "artifact_label",
            "request_mark",
            "request_verifier",
            "mark",
            "verifier",
            "request_ttl_ms",
            "max_pending_requests",
            "allowed_actions",
        },
        set(),
        "email authorization_control",
    )
    for key in (
        "source_interface",
        "outbound_interface",
        "request_checkpoint",
        "checkpoint",
        "request_artifact_label",
        "artifact_label",
        "request_mark",
        "request_verifier",
        "mark",
        "verifier",
    ):
        require_string(authorization_control[key], f"email authorization_control.{key}")
    require_int(authorization_control["request_ttl_ms"], "email authorization_control.request_ttl_ms", 60000, 604800000)
    require_int(authorization_control["max_pending_requests"], "email authorization_control.max_pending_requests", 1, 10000)
    authorization_actions = require_string_list(
        authorization_control["allowed_actions"],
        "email authorization_control.allowed_actions",
    )
    unsupported_authorization_actions = sorted(
        set(authorization_actions) - SUPPORTED_AUTHORIZATION_CONTROL_ACTIONS
    )
    if unsupported_authorization_actions:
        raise ContractError(
            "email authorization_control declares unsupported actions: "
            + ", ".join(unsupported_authorization_actions)
        )
    scopes = require_keys(
        cfg["scopes"],
        {
            "read_mailboxes",
            "write_mailboxes",
            "manage_mailboxes",
            "allow_all_recipients_env",
            "allowed_recipients_env",
            "allowed_recipient_domains_env",
            "deny_unmatched_recipients",
        },
        set(),
        "email scopes",
    )
    for key in ("read_mailboxes", "write_mailboxes"):
        patterns = require_string_list(scopes[key], f"email scopes.{key}")
        for index, pattern in enumerate(patterns):
            if any(character in pattern for character in ("\x00", "\r", "\n")):
                raise ContractError(f"email scopes.{key}[{index}] contains forbidden characters")
    require_bool(scopes["manage_mailboxes"], "email scopes.manage_mailboxes")
    for key in ("allow_all_recipients_env", "allowed_recipients_env", "allowed_recipient_domains_env"):
        _validate_env_name(scopes[key], f"email scopes.{key}")
    require_bool(scopes["deny_unmatched_recipients"], "email scopes.deny_unmatched_recipients")
    content = require_keys(
        cfg["content"],
        {
            "max_raw_message_bytes",
            "max_body_chars",
            "max_header_chars",
            "max_headers",
            "max_attachment_bytes",
            "max_total_attachment_bytes",
            "max_attachments",
            "max_mime_parts",
            "max_inline_data_bytes",
            "preserve_raw_message",
            "include_html",
            "skip_attachments",
        },
        set(),
        "email content",
    )
    for key, minimum, maximum in (
        ("max_raw_message_bytes", 1024, 1073741824),
        ("max_body_chars", 1, 10000000),
        ("max_header_chars", 64, 1048576),
        ("max_headers", 1, 10000),
        ("max_attachment_bytes", 0, 1073741824),
        ("max_total_attachment_bytes", 0, 2147483648),
        ("max_attachments", 0, 10000),
        ("max_mime_parts", 1, 100000),
        ("max_inline_data_bytes", 0, 67108864),
    ):
        require_int(content[key], f"email content.{key}", minimum, maximum)
    if content["max_total_attachment_bytes"] < content["max_attachment_bytes"]:
        raise ContractError("email total attachment bound must cover one attachment")
    for key in ("preserve_raw_message", "include_html", "skip_attachments"):
        require_bool(content[key], f"email content.{key}")
    composition = require_keys(
        cfg["composition"],
        {
            "signature_env",
            "signature_separator",
            "reply_quote_original",
            "forward_include_original",
            "allow_custom_headers",
            "protected_headers",
        },
        set(),
        "email composition",
    )
    _validate_env_name(composition["signature_env"], "email composition.signature_env")
    separator = composition["signature_separator"]
    if not isinstance(separator, str) or "\x00" in separator or "\r" in separator or "\n" in separator or len(separator) > 64:
        raise ContractError("email composition.signature_separator must be a short single-line string")
    require_bool(composition["reply_quote_original"], "email composition.reply_quote_original")
    require_bool(composition["forward_include_original"], "email composition.forward_include_original")
    require_bool(composition["allow_custom_headers"], "email composition.allow_custom_headers")
    protected_headers = require_string_list(composition["protected_headers"], "email composition.protected_headers")
    for index, header in enumerate(protected_headers):
        if not re.fullmatch(r"[A-Za-z0-9-]{1,128}", header):
            raise ContractError(f"email composition.protected_headers[{index}] is invalid")
    outbound = require_keys(
        cfg["outbound"],
        {
            "accepted_labels",
            "allowed_actions",
            "max_recipients",
            "max_subject_chars",
            "max_body_chars",
            "max_message_bytes",
            "max_attachment_bytes",
            "max_total_attachment_bytes",
            "max_attachments",
            "attachment_roots_env",
            "min_action_interval_ms",
            "retry_max_attempts",
            "retry_backoff_ms",
            "retry_backoff_cap_ms",
            "ledger_batch_size",
        },
        set(),
        "email outbound",
    )
    require_string_list(outbound["accepted_labels"], "email outbound.accepted_labels")
    outbound_actions = require_string_list(outbound["allowed_actions"], "email outbound.allowed_actions")
    unsupported_outbound_actions = sorted(set(outbound_actions) - SUPPORTED_OUTBOUND_ACTIONS)
    if unsupported_outbound_actions:
        raise ContractError(
            "email outbound declares unsupported actions: "
            + ", ".join(unsupported_outbound_actions)
        )
    require_int(outbound["max_recipients"], "email outbound.max_recipients", 1, 10000)
    require_int(outbound["max_subject_chars"], "email outbound.max_subject_chars", 1, 998)
    require_int(outbound["max_body_chars"], "email outbound.max_body_chars", 1, 10000000)
    require_int(outbound["max_message_bytes"], "email outbound.max_message_bytes", 1024, 1073741824)
    require_int(outbound["max_attachment_bytes"], "email outbound.max_attachment_bytes", 0, 1073741824)
    require_int(outbound["max_total_attachment_bytes"], "email outbound.max_total_attachment_bytes", 0, 2147483648)
    require_int(outbound["max_attachments"], "email outbound.max_attachments", 0, 10000)
    if outbound["max_attachment_bytes"] > outbound["max_total_attachment_bytes"]:
        raise ContractError("email outbound total attachment bound must cover one attachment")
    _validate_env_name(outbound["attachment_roots_env"], "email outbound.attachment_roots_env")
    require_int(outbound["min_action_interval_ms"], "email outbound.min_action_interval_ms", 0, 60000)
    attempts = require_int(outbound["retry_max_attempts"], "email outbound.retry_max_attempts", 1, 100)
    initial = require_int(outbound["retry_backoff_ms"], "email outbound.retry_backoff_ms", 1, 3600000)
    cap = require_int(outbound["retry_backoff_cap_ms"], "email outbound.retry_backoff_cap_ms", initial, 86400000)
    if attempts and cap < initial:
        raise ContractError("email outbound retry cap must not be below initial backoff")
    require_int(outbound["ledger_batch_size"], "email outbound.ledger_batch_size", 1, 1000)
    ipc = require_keys(
        cfg["driver_ipc"],
        {"verdict_timeout_ms", "idle_sleep_ms", "max_outbound_payload_bytes", "max_cached_manifest_bytes", "ledger_fallback_max_line_bytes"},
        set(),
        "email driver_ipc",
    )
    require_int(ipc["verdict_timeout_ms"], "email driver_ipc.verdict_timeout_ms", 1000, 600000)
    require_int(ipc["idle_sleep_ms"], "email driver_ipc.idle_sleep_ms", 10, 60000)
    require_int(ipc["max_outbound_payload_bytes"], "email driver_ipc.max_outbound_payload_bytes", 1024, 1073741824)
    require_int(ipc["max_cached_manifest_bytes"], "email driver_ipc.max_cached_manifest_bytes", 1024, 67108864)
    require_int(ipc["ledger_fallback_max_line_bytes"], "email driver_ipc.ledger_fallback_max_line_bytes", 1024, 1073741824)
    template = require_keys(
        cfg["gateway_run_template"],
        {"id", "ee", "worker", "trace", "status", "interface_state", "checkpoints", "active_checkpoints"},
        set(),
        "email gateway_run_template",
    )
    for key in ("id", "ee", "worker", "status"):
        require_string(template[key], f"email gateway_run_template.{key}")
    mandatory_role_bindings = worker_role_artifact_bindings(template["worker"])
    require_string(template["trace"], "email gateway_run_template.trace")
    if not isinstance(template["interface_state"], dict):
        raise ContractError("email gateway_run_template.interface_state must be an object")
    if not isinstance(template["checkpoints"], list) or not template["checkpoints"]:
        raise ContractError("email gateway_run_template.checkpoints must be a non-empty array")
    require_string_list(template["active_checkpoints"], "email gateway_run_template.active_checkpoints")
    checkpoints_by_id = {}
    for index, checkpoint in enumerate(template["checkpoints"]):
        checkpoint = require_keys(
            checkpoint,
            {"id", "run", "trace", "epoch", "interface_binding", "outbound_interface_bindings", "artifact_bindings", "required_verified_artifacts"},
            set(),
            f"email gateway_run_template.checkpoints[{index}]",
        )
        checkpoint_id = require_string(checkpoint.get("id"), f"email gateway_run_template.checkpoints[{index}].id")
        if checkpoint_id in checkpoints_by_id:
            raise ContractError(f"email gateway_run_template repeats checkpoint {checkpoint_id!r}")
        if checkpoint["run"] != template["id"]:
            raise ContractError(f"email gateway_run_template checkpoint {checkpoint_id!r} belongs to another run")
        require_string(checkpoint["trace"], f"email gateway_run_template checkpoint {checkpoint_id}.trace")
        require_int(checkpoint["epoch"], f"email gateway_run_template checkpoint {checkpoint_id}.epoch", 0)
        require_interface_binding(
            checkpoint["interface_binding"],
            f"email gateway_run_template checkpoint {checkpoint_id}.interface_binding",
        )
        outbound_bindings = checkpoint["outbound_interface_bindings"]
        if not isinstance(outbound_bindings, list):
            raise ContractError(f"email gateway_run_template checkpoint {checkpoint_id!r} outbound bindings must be an array")
        for outbound_index, outbound_binding in enumerate(outbound_bindings):
            require_interface_binding(
                outbound_binding,
                f"email gateway_run_template checkpoint {checkpoint_id}.outbound_interface_bindings[{outbound_index}]",
            )
        if not isinstance(checkpoint["artifact_bindings"], list) or not isinstance(checkpoint["required_verified_artifacts"], list):
            raise ContractError(f"email gateway_run_template checkpoint {checkpoint_id!r} artifact fields must be arrays")
        artifact_bindings = [
            require_keys(binding, {"id"}, set(), f"email gateway_run_template checkpoint {checkpoint_id}.artifact_bindings[{binding_index}]")
            for binding_index, binding in enumerate(checkpoint["artifact_bindings"])
        ]
        for binding_index, binding in enumerate(artifact_bindings):
            require_string(binding["id"], f"email gateway_run_template checkpoint {checkpoint_id}.artifact_bindings[{binding_index}].id")
        if artifact_bindings[:len(mandatory_role_bindings)] != mandatory_role_bindings:
            raise ContractError(f"email gateway_run_template checkpoint {checkpoint_id!r} omits mandatory worker role bindings")
        checkpoints_by_id[checkpoint_id] = checkpoint

    def require_authorization_checkpoint(checkpoint_key, source_interface, outbound_interface, label_key, mark_key, verifier_key):
        checkpoint_id = authorization_control[checkpoint_key]
        checkpoint = checkpoints_by_id.get(checkpoint_id)
        if checkpoint is None:
            raise ContractError(f"email gateway_run_template is missing {checkpoint_id!r}")
        checkpoint = require_keys(
            checkpoint,
            {"id", "run", "trace", "epoch", "interface_binding", "outbound_interface_bindings", "artifact_bindings", "required_verified_artifacts"},
            set(),
            f"email authorization checkpoint {checkpoint_id}",
        )
        if checkpoint["run"] != template["id"]:
            raise ContractError(f"email authorization checkpoint {checkpoint_id!r} run does not match the gateway run")
        require_string(checkpoint["trace"], f"email authorization checkpoint {checkpoint_id}.trace")
        require_int(checkpoint["epoch"], f"email authorization checkpoint {checkpoint_id}.epoch", 0)
        if require_interface_binding(
            checkpoint["interface_binding"],
            f"email authorization checkpoint {checkpoint_id}.interface_binding",
        ) != source_interface:
            raise ContractError(f"email authorization checkpoint {checkpoint_id!r} uses the wrong source interface")
        outbound_bindings = checkpoint["outbound_interface_bindings"]
        if not isinstance(outbound_bindings, list) or len(outbound_bindings) != 1:
            raise ContractError(f"email authorization checkpoint {checkpoint_id!r} must have one outbound interface")
        if require_interface_binding(
            outbound_bindings[0],
            f"email authorization checkpoint {checkpoint_id}.outbound_interface_bindings[0]",
        ) != outbound_interface:
            raise ContractError(f"email authorization checkpoint {checkpoint_id!r} uses the wrong outbound interface")
        if checkpoint["artifact_bindings"] != mandatory_role_bindings:
            raise ContractError(f"email authorization checkpoint {checkpoint_id!r} must not bind unrelated artifacts")
        requirements = checkpoint["required_verified_artifacts"]
        if not isinstance(requirements, list) or len(requirements) != 1:
            raise ContractError(f"email authorization checkpoint {checkpoint_id!r} must carry exactly one verified requirement")
        requirement = require_keys(
            requirements[0],
            {"mark", "artifact_label", "verifier", "params"},
            set(),
            f"email authorization checkpoint {checkpoint_id} requirement",
        )
        if requirement["mark"] != authorization_control[mark_key]:
            raise ContractError(f"email authorization checkpoint {checkpoint_id!r} mark drifted")
        if requirement["artifact_label"] != authorization_control[label_key]:
            raise ContractError(f"email authorization checkpoint {checkpoint_id!r} artifact label drifted")
        if requirement["verifier"] != authorization_control[verifier_key]:
            raise ContractError(f"email authorization checkpoint {checkpoint_id!r} verifier drifted")
        if not isinstance(requirement["params"], dict):
            raise ContractError(f"email authorization checkpoint {checkpoint_id!r} verifier params must be an object")
        if checkpoint_id not in template["active_checkpoints"]:
            raise ContractError(f"email authorization checkpoint {checkpoint_id!r} must be active")

    require_authorization_checkpoint(
        "request_checkpoint",
        authorization_control["outbound_interface"],
        authorization_control["source_interface"],
        "request_artifact_label",
        "request_mark",
        "request_verifier",
    )
    require_authorization_checkpoint(
        "checkpoint",
        authorization_control["source_interface"],
        authorization_control["outbound_interface"],
        "artifact_label",
        "mark",
        "verifier",
    )
    if set(authorization_actions) & set(outbound_actions):
        raise ContractError("email operator authorization actions must not be available on the worker output path")
    return cfg


def load_config(path: str) -> dict[str, Any]:
    with open(path, "r", encoding="utf-8") as handle:
        return validate_config(json.load(handle))


def env_flag(name: str, environ: dict[str, str] | None = None) -> bool:
    source = os.environ if environ is None else environ
    raw = source.get(name, "").strip().lower()
    if not raw:
        return False
    if raw in {"1", "true", "yes", "on"}:
        return True
    if raw in {"0", "false", "no", "off"}:
        return False
    raise ContractError(f"{name} must be a boolean environment value")


def env_csv(name: str, environ: dict[str, str] | None = None) -> list[str]:
    source = os.environ if environ is None else environ
    raw = source.get(name, "")
    values: list[str] = []
    seen: set[str] = set()
    for item in raw.split(","):
        value = item.strip()
        if not value:
            continue
        lowered = value.lower()
        if lowered in seen:
            continue
        seen.add(lowered)
        values.append(value)
    return values


def decode_header_value(value: str | None, limit: int | None = None) -> str:
    if not value:
        return ""
    output: list[str] = []
    for part, charset in decode_header(value):
        if isinstance(part, bytes):
            candidates = [charset, "utf-8", "latin-1"]
            decoded = None
            for candidate in candidates:
                if not candidate:
                    continue
                try:
                    decoded = part.decode(candidate, errors="strict")
                    break
                except (LookupError, UnicodeDecodeError):
                    continue
            output.append(decoded if decoded is not None else part.decode("utf-8", errors="replace"))
        else:
            output.append(part)
    text = "".join(output).replace("\r", " ").replace("\n", " ").strip()
    return text if limit is None else text[:limit]


def normalize_address(value: str) -> str:
    text = value.strip()
    if not _ADDRESS_RE.fullmatch(text):
        raise ContractError(f"invalid email address {value!r}")
    local, domain = text.rsplit("@", 1)
    try:
        encoded_domain = domain.encode("idna").decode("ascii")
    except UnicodeError as error:
        raise ContractError(f"invalid email domain in {value!r}") from error
    if len(local.encode("utf-8")) > 64 or len(encoded_domain) > 255:
        raise ContractError(f"email address is too long: {value!r}")
    return f"{local}@{encoded_domain.lower()}"


def parse_addresses(values: Iterable[str | None], limit: int = 1000) -> list[dict[str, str]]:
    raw = [value for value in values if value]
    parsed = getaddresses(raw)
    if len(parsed) > limit:
        raise ContractError(f"email header contains more than {limit} addresses")
    result: list[dict[str, str]] = []
    seen: set[str] = set()
    for display_name, address in parsed:
        if not address:
            continue
        normalized = normalize_address(address)
        identity = normalized.lower()
        if identity in seen:
            continue
        seen.add(identity)
        result.append({"address": normalized, "name": decode_header_value(display_name, 4096)})
    return result


def safe_message_id(value: str | None) -> str | None:
    if not value:
        return None
    text = decode_header_value(value, 4096)
    return text if _MESSAGE_ID_RE.fullmatch(text) else None


def parse_message_ids(value: str | None) -> list[str]:
    if not value:
        return []
    result: list[str] = []
    seen: set[str] = set()
    for candidate in re.findall(r"<[^<>\r\n]+>", value):
        if candidate not in seen:
            seen.add(candidate)
            result.append(candidate)
    return result


def normalize_subject(value: str) -> str:
    current = decode_header_value(value, 16384)
    while True:
        updated = re.sub(r"^\s*(?:re|fw|fwd)(?:\[\d+\])?\s*:\s*", "", current, flags=re.IGNORECASE)
        if updated == current:
            break
        current = updated
    return re.sub(r"\s+", " ", current).strip().lower()


def thread_key(message_id: str | None, references: list[str], in_reply_to: str | None, subject: str, participants: list[str]) -> str:
    anchor = references[0] if references else in_reply_to or message_id
    if anchor:
        material = f"message-id:{anchor.lower()}"
    else:
        material = json.dumps(
            {"subject": normalize_subject(subject), "participants": sorted({value.lower() for value in participants})},
            separators=(",", ":"),
            sort_keys=True,
        )
    return hashlib.sha256(material.encode("utf-8")).hexdigest()


def html_to_text(value: str) -> str:
    parser = HtmlTextExtractor()
    try:
        parser.feed(value)
        parser.close()
        return parser.text()
    except Exception:
        text = re.sub(r"<[^>]+>", " ", value)
        return re.sub(r"\s+", " ", html.unescape(text)).strip()


def _decoded_part(part: Message, max_bytes: int) -> tuple[bytes, bool]:
    payload = part.get_payload(decode=True)
    if payload is None:
        raw = part.get_payload()
        if isinstance(raw, str):
            payload = raw.encode(part.get_content_charset() or "utf-8", errors="replace")
        else:
            payload = b""
    truncated = len(payload) > max_bytes
    return payload[:max_bytes], truncated


def _decode_text_part(part: Message, max_chars: int) -> tuple[str, bool]:
    max_bytes = min(max_chars * 4 + 4096, 67108864)
    payload, bytes_truncated = _decoded_part(part, max_bytes)
    charset = part.get_content_charset() or "utf-8"
    try:
        text = payload.decode(charset, errors="replace")
    except LookupError:
        text = payload.decode("utf-8", errors="replace")
    truncated = bytes_truncated or len(text) > max_chars
    return text[:max_chars], truncated


def safe_filename(value: str | None, fallback: str) -> str:
    decoded = decode_header_value(value, 4096) if value else fallback
    decoded = decoded.replace("\\", "/").split("/")[-1]
    decoded = re.sub(r"[\x00-\x1f\x7f]", "_", decoded).strip().strip(".")
    if not decoded:
        decoded = fallback
    stem, suffix = os.path.splitext(decoded)
    digest = hashlib.sha256(decoded.encode("utf-8")).hexdigest()[:12]
    stem = stem[:180] or "attachment"
    suffix = suffix[:32]
    return f"{stem}-{digest}{suffix}"


def safe_display_filename(value: str | None, fallback: str) -> str:
    decoded = decode_header_value(value, 4096) if value else fallback
    decoded = decoded.replace("\\", "/").split("/")[-1]
    decoded = re.sub(r"[\x00-\x1f\x7f]", "_", decoded).strip().strip(".")
    if not decoded:
        decoded = fallback
    stem, suffix = os.path.splitext(decoded)
    return f"{(stem[:220] or 'attachment')}{suffix[:32]}"


def atomic_write(path: str, payload: bytes, mode: int = 0o600) -> None:
    parent = os.path.dirname(path)
    os.makedirs(parent, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{os.path.basename(path)}.", suffix=".tmp", dir=parent)
    try:
        os.fchmod(descriptor, mode)
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


def persist_content_addressed(directory: str, payload: bytes, suffix: str = "") -> tuple[str, str]:
    digest = hashlib.sha256(payload).hexdigest()
    normalized_suffix = re.sub(r"[^A-Za-z0-9._-]", "_", suffix)[:64]
    path = os.path.join(directory, f"{digest}{normalized_suffix}")
    if os.path.exists(path):
        with open(path, "rb") as handle:
            if hashlib.sha256(handle.read()).hexdigest() != digest:
                raise ContractError(f"content-addressed path {path!r} does not match its digest")
    else:
        atomic_write(path, payload)
    return path, digest


def parse_date(value: str | None) -> str | None:
    if not value:
        return None
    try:
        return parsedate_to_datetime(value).isoformat()
    except (TypeError, ValueError, OverflowError):
        return None


def automated_sender(config: dict[str, Any], sender: str, message: Message) -> tuple[bool, str | None]:
    lowered = sender.lower()
    for fragment in config["authz"]["automated_sender_fragments"]:
        if fragment.lower() in lowered:
            return True, f"sender_fragment:{fragment}"
    for header, blocked_values in config["authz"]["automated_headers"].items():
        actual = decode_header_value(message.get(header), 4096).lower()
        if actual and any(blocked.lower() in actual for blocked in blocked_values):
            return True, f"header:{header}"
    if config["authz"]["deny_list_unsubscribe"] and message.get("List-Unsubscribe"):
        return True, "header:List-Unsubscribe"
    return False, None


def sender_authorized(config: dict[str, Any], sender: str, approved: set[str], environ: dict[str, str] | None = None) -> bool:
    authz = config["authz"]
    normalized = normalize_address(sender).lower()
    if env_flag(authz["allow_all_env"], environ):
        return True
    users = {normalize_address(value).lower() for value in env_csv(authz["allowed_users_env"], environ)}
    domains = {value.lower().lstrip("@") for value in env_csv(authz["allowed_domains_env"], environ)}
    if normalized in users or normalized in approved:
        return True
    domain = normalized.rsplit("@", 1)[1]
    return domain in domains


def _header_values(message: Message, name: str) -> list[str]:
    return [str(value) for value in message.get_all(name, [])]


def parse_inbound_message(
    raw: bytes,
    config: dict[str, Any],
    media_dir: str,
    raw_dir: str,
    account_address: str,
    mailbox: str,
    uidvalidity: int,
    uid: int,
    flags: list[str],
    internal_date: str | None,
) -> dict[str, Any]:
    limits = config["content"]
    if len(raw) > limits["max_raw_message_bytes"]:
        raise ContractError(f"email message exceeds max_raw_message_bytes ({len(raw)} bytes)")
    message = BytesParser(policy=policy.default).parsebytes(raw)
    raw_headers = list(message.raw_items())
    if len(raw_headers) > limits["max_headers"]:
        raise ContractError("email message exceeds max_headers")
    for header_name, header_value in raw_headers:
        if len(header_name) > 128 or len(header_value) > limits["max_header_chars"]:
            raise ContractError("email message contains an oversized header")
    from_values = parse_addresses(_header_values(message, "From"), 100)
    if len(from_values) != 1:
        raise ContractError("email message must have exactly one valid From address")
    sender = from_values[0]
    recipient_groups = {
        name.lower(): parse_addresses(_header_values(message, name), 1000)
        for name in ("To", "Cc", "Bcc", "Reply-To")
    }
    subject = decode_header_value(message.get("Subject"), limits["max_header_chars"])
    message_id = safe_message_id(message.get("Message-ID"))
    in_reply_to_ids = parse_message_ids(message.get("In-Reply-To"))
    references = parse_message_ids(message.get("References"))
    text_parts: list[str] = []
    html_parts: list[str] = []
    text_truncated = False
    html_truncated = False
    attachments: list[dict[str, Any]] = []
    rejected_attachments: list[dict[str, Any]] = []
    total_attachment_bytes = 0
    attachment_index = 0
    mime_part_count = 0
    for part in message.walk():
        mime_part_count += 1
        if mime_part_count > limits["max_mime_parts"]:
            raise ContractError("email message exceeds max_mime_parts")
        if part.is_multipart():
            continue
        media_type = part.get_content_type().lower()
        disposition = (part.get_content_disposition() or "").lower()
        filename = part.get_filename()
        is_attachment = bool(filename) or disposition in {"attachment", "inline"}
        if media_type == "text/plain" and not is_attachment:
            value, truncated = _decode_text_part(part, limits["max_body_chars"])
            text_parts.append(value)
            text_truncated = text_truncated or truncated
            continue
        if media_type == "text/html" and not is_attachment:
            value, truncated = _decode_text_part(part, limits["max_body_chars"])
            html_parts.append(value)
            html_truncated = html_truncated or truncated
            continue
        if not is_attachment:
            continue
        attachment_index += 1
        if limits["skip_attachments"]:
            rejected_attachments.append({"index": attachment_index, "reason": "attachments_disabled", "media_type": media_type})
            continue
        if len(attachments) >= limits["max_attachments"]:
            rejected_attachments.append({"index": attachment_index, "reason": "attachment_count_limit", "media_type": media_type})
            continue
        payload, truncated = _decoded_part(part, limits["max_attachment_bytes"] + 1)
        if truncated or len(payload) > limits["max_attachment_bytes"]:
            rejected_attachments.append({"index": attachment_index, "reason": "attachment_size_limit", "size_at_least": len(payload), "media_type": media_type})
            continue
        if total_attachment_bytes + len(payload) > limits["max_total_attachment_bytes"]:
            rejected_attachments.append({"index": attachment_index, "reason": "total_attachment_size_limit", "size": len(payload), "media_type": media_type})
            continue
        total_attachment_bytes += len(payload)
        original_filename = decode_header_value(filename, 4096) if filename else f"attachment-{attachment_index}"
        stored_name = safe_filename(original_filename, f"attachment-{attachment_index}")
        suffix = Path(stored_name).suffix
        stored_path, digest = persist_content_addressed(media_dir, payload, suffix)
        content_id = decode_header_value(part.get("Content-ID"), 4096) or None
        attachments.append(
            {
                "index": attachment_index,
                "filename": original_filename,
                "stored_path": stored_path,
                "sha256": digest,
                "size": len(payload),
                "media_type": media_type,
                "content_disposition": disposition or "attachment",
                "content_id": content_id,
                "content_location": decode_header_value(part.get("Content-Location"), 4096) or None,
                "transfer_encoding": decode_header_value(part.get("Content-Transfer-Encoding"), 128) or None,
            }
        )
    body_text = "\n\n".join(value.strip() for value in text_parts if value.strip())
    html_body = "\n".join(value for value in html_parts if value)
    if not body_text and html_body:
        body_text = html_to_text(html_body)
    body_truncated = text_truncated or len(body_text) > limits["max_body_chars"]
    body_text = body_text[: limits["max_body_chars"]]
    if len(html_body) > limits["max_body_chars"]:
        html_truncated = True
        html_body = html_body[: limits["max_body_chars"]]
    if not limits["include_html"]:
        html_body = ""
    raw_path = None
    raw_sha256 = hashlib.sha256(raw).hexdigest()
    if limits["preserve_raw_message"]:
        raw_path, raw_sha256 = persist_content_addressed(raw_dir, raw, ".eml")
    all_participants = [sender["address"]]
    for group in recipient_groups.values():
        all_participants.extend(item["address"] for item in group)
    received_id = message_id or f"<octopus-import-{raw_sha256[:32]}@local.invalid>"
    return {
        "schema_version": 1,
        "account": config["account"]["id"],
        "mailbox": mailbox,
        "uidvalidity": uidvalidity,
        "uid": uid,
        "message_id": received_id,
        "message_id_synthetic": message_id is None,
        "in_reply_to": in_reply_to_ids,
        "references": references,
        "thread_key": thread_key(message_id, references, in_reply_to_ids[-1] if in_reply_to_ids else None, subject, all_participants),
        "from": sender,
        "to": recipient_groups["to"],
        "cc": recipient_groups["cc"],
        "bcc": recipient_groups["bcc"],
        "reply_to": recipient_groups["reply-to"],
        "subject": subject,
        "date": parse_date(message.get("Date")),
        "internal_date": internal_date,
        "flags": sorted(set(flags)),
        "body": {
            "text": body_text,
            "html": html_body or None,
            "text_truncated": body_truncated,
            "html_truncated": html_truncated,
        },
        "attachments": attachments,
        "rejected_attachments": rejected_attachments,
        "raw": {"path": raw_path, "sha256": raw_sha256, "size": len(raw)},
        "received_for": normalize_address(account_address),
    }


def require_action_payload(payload: Any, allowed_actions: set[str]) -> tuple[str, dict[str, Any]]:
    obj = require_object(payload, "email action artifact")
    action = require_string(obj.get("action"), "email action")
    if action not in allowed_actions:
        raise ContractError(f"email action {action!r} is not allowed by the interface descriptor")
    return action, obj


def require_uid(value: Any, name: str = "uid") -> int:
    return require_int(value, name, 1, 4294967295)


def require_mailbox(value: Any, aliases: dict[str, str], name: str = "mailbox") -> str:
    text = require_string(value, name)
    resolved = aliases.get(text, text)
    if any(character in resolved for character in ("\x00", "\r", "\n")):
        raise ContractError(f"{name} contains forbidden characters")
    return resolved


def require_flag(value: Any) -> str:
    text = require_string(value, "email flag")
    if not _SAFE_FLAG_RE.fullmatch(text):
        raise ContractError(f"invalid email flag {text!r}")
    return text


def require_label(value: Any) -> str:
    text = require_string(value, "email label")
    if not _SAFE_LABEL_RE.fullmatch(text):
        raise ContractError(f"invalid email label {text!r}")
    return text


def _recipient_header(items: Any, name: str, max_recipients: int) -> list[str]:
    if items is None:
        return []
    if isinstance(items, str):
        raw = [items]
    elif isinstance(items, list) and all(isinstance(item, str) for item in items):
        raw = items
    else:
        raise ContractError(f"{name} must be a string or array of strings")
    parsed = parse_addresses(raw, max_recipients)
    if len(parsed) > max_recipients:
        raise ContractError(f"{name} exceeds the recipient limit")
    return [f"{item['name']} <{item['address']}>" if item["name"] else item["address"] for item in parsed]


def _clean_header(value: Any, name: str, limit: int) -> str:
    text = require_string(value, name, True)
    if "\r" in text or "\n" in text:
        raise ContractError(f"{name} must not contain line breaks")
    if len(text) > limit:
        raise ContractError(f"{name} exceeds {limit} characters")
    return text


def deterministic_message_id(address: str, source_key: str) -> str:
    domain = normalize_address(address).rsplit("@", 1)[1]
    digest = hashlib.sha256(source_key.encode("utf-8")).hexdigest()
    return f"<octopus-{digest[:40]}@{domain}>"


def _attachment_from_payload(
    item: Any,
    resolver: Callable[[dict[str, Any]], tuple[bytes, dict[str, Any]]],
    index: int,
    max_bytes: int,
) -> ResolvedAttachment:
    obj = require_object(item, f"attachments[{index}]")
    data, evidence = resolver(obj)
    if len(data) > max_bytes:
        raise ContractError(f"attachments[{index}] exceeds the attachment size limit")
    filename = safe_display_filename(obj.get("filename") if isinstance(obj.get("filename"), str) else None, f"attachment-{index}")
    media_type = obj.get("media_type", "application/octet-stream")
    media_type = require_string(media_type, f"attachments[{index}].media_type")
    if "/" not in media_type or any(char in media_type for char in "\r\n;"):
        raise ContractError(f"attachments[{index}].media_type is invalid")
    disposition = obj.get("disposition", "attachment")
    if disposition not in {"attachment", "inline"}:
        raise ContractError(f"attachments[{index}].disposition must be attachment or inline")
    content_id = obj.get("content_id")
    if content_id is not None:
        content_id = _clean_header(content_id, f"attachments[{index}].content_id", 4096)
    return ResolvedAttachment(data, filename, media_type, disposition, content_id, evidence)


def build_outbound_message(
    action: str,
    payload: dict[str, Any],
    account_address: str,
    display_name: str,
    source_key: str,
    outbound_config: dict[str, Any],
    composition_config: dict[str, Any],
    resolver: Callable[[dict[str, Any]], tuple[bytes, dict[str, Any]]],
    original: dict[str, Any] | None = None,
) -> tuple[EmailMessage, list[str], list[dict[str, Any]]]:
    max_recipients = outbound_config["max_recipients"]
    to = _recipient_header(payload.get("to"), "to", max_recipients)
    cc = _recipient_header(payload.get("cc"), "cc", max_recipients)
    bcc = _recipient_header(payload.get("bcc"), "bcc", max_recipients)
    if action in {"message.reply", "message.reply_all", "message.forward"} and original is None:
        raise ContractError(f"{action} requires an original message")
    account_lower = normalize_address(account_address).lower()
    if action in {"message.reply", "message.reply_all"}:
        sender = original["from"]["address"]
        reply_to = original.get("reply_to") or []
        primary = reply_to[0]["address"] if reply_to else sender
        if not to:
            to = [primary]
        if action == "message.reply_all":
            candidates: list[str] = []
            for group in (original.get("to", []), original.get("cc", [])):
                for item in group:
                    if item["address"].lower() != account_lower:
                        candidates.append(item["address"])
            existing = {item["address"].lower() for item in parse_addresses(to + cc, max_recipients)}
            for candidate in candidates:
                if candidate.lower() not in existing:
                    cc.append(candidate)
                    existing.add(candidate.lower())
    if action == "message.forward" and not to:
        raise ContractError("message.forward requires recipients")
    if not to:
        raise ContractError("outbound email requires at least one To recipient")
    envelope_recipients = parse_addresses(to + cc + bcc, max_recipients)
    if len(envelope_recipients) > max_recipients:
        raise ContractError("outbound email exceeds max_recipients")
    subject_default = original.get("subject", "") if original else ""
    subject = payload.get("subject", subject_default)
    subject = _clean_header(subject, "subject", outbound_config["max_subject_chars"])
    if action in {"message.reply", "message.reply_all"} and not re.match(r"^\s*re\s*:", subject, flags=re.IGNORECASE):
        subject = f"Re: {subject}"
    if action == "message.forward" and not re.match(r"^\s*(?:fwd?|fw)\s*:", subject, flags=re.IGNORECASE):
        subject = f"Fwd: {subject}"
    text_body = payload.get("text", payload.get("body", ""))
    text_body = require_text(text_body, "email body")
    html_body = payload.get("html")
    if html_body is not None:
        html_body = require_text(html_body, "email html body")
    if len(text_body) > outbound_config["max_body_chars"] or (html_body is not None and len(html_body) > outbound_config["max_body_chars"]):
        raise ContractError("outbound email body exceeds max_body_chars")
    quote_original = payload.get("quote_original", composition_config["reply_quote_original"])
    if not isinstance(quote_original, bool):
        raise ContractError("quote_original must be a boolean")
    include_original = payload.get("include_original", composition_config["forward_include_original"])
    if not isinstance(include_original, bool):
        raise ContractError("include_original must be a boolean")
    if original and action in {"message.reply", "message.reply_all"} and quote_original:
        sender_name = original["from"].get("name") or original["from"]["address"]
        heading = f"On {original.get('date') or original.get('internal_date') or 'an earlier message'}, {sender_name} wrote:"
        quoted = "\n".join(f"> {line}" if line else ">" for line in original["body"]["text"].splitlines())
        text_body = f"{text_body.rstrip()}\n\n{heading}\n{quoted}" if text_body else f"{heading}\n{quoted}"
        if html_body is not None and original["body"].get("html"):
            html_body = f"{html_body}<br><br><div>{html.escape(heading)}</div><blockquote>{original['body']['html']}</blockquote>"
    if original and action == "message.forward" and include_original:
        sender_name = original["from"].get("name") or original["from"]["address"]
        headers = [
            "---------- Forwarded message ----------",
            f"From: {sender_name} <{original['from']['address']}>",
            f"Date: {original.get('date') or original.get('internal_date') or ''}",
            f"Subject: {original.get('subject') or ''}",
            "To: " + ", ".join(item["address"] for item in original.get("to", [])),
        ]
        forwarded = "\n".join(headers) + "\n\n" + original["body"]["text"]
        text_body = f"{text_body.rstrip()}\n\n{forwarded}" if text_body else forwarded
        if html_body is not None and original["body"].get("html"):
            html_headers = "<br>".join(html.escape(line) for line in headers)
            html_body = f"{html_body}<br><br><div>{html_headers}</div><blockquote>{original['body']['html']}</blockquote>"
    signature = os.environ.get(composition_config["signature_env"], "")
    if signature:
        if "\x00" in signature:
            raise ContractError("email signature must not contain NUL")
        signature = signature.replace("\r\n", "\n").replace("\r", "\n")
        separator = composition_config["signature_separator"]
        text_body = f"{text_body.rstrip()}\n\n{separator}\n{signature}" if text_body else f"{separator}\n{signature}"
        if html_body is not None:
            rendered_signature = html.escape(signature).replace("\n", "<br>")
            html_body = f"{html_body}<br><br>{html.escape(separator)}<br>{rendered_signature}"
    if len(text_body) > outbound_config["max_body_chars"] or (html_body is not None and len(html_body) > outbound_config["max_body_chars"]):
        raise ContractError("outbound email body exceeds max_body_chars after composition")
    message = EmailMessage(policy=policy.SMTP)
    message["From"] = f"{display_name} <{account_address}>" if display_name else account_address
    message["To"] = ", ".join(to)
    if cc:
        message["Cc"] = ", ".join(cc)
    reply_to_header = _recipient_header(payload.get("reply_to"), "reply_to", max_recipients)
    if reply_to_header:
        message["Reply-To"] = ", ".join(reply_to_header)
    message["Subject"] = subject
    message["Date"] = format_datetime(__import__("datetime").datetime.now(__import__("datetime").timezone.utc))
    message_id = payload.get("message_id")
    if message_id is not None:
        message_id = _clean_header(message_id, "message_id", 4096)
        if not _MESSAGE_ID_RE.fullmatch(message_id):
            raise ContractError("message_id must be an RFC Message-ID enclosed in angle brackets")
    else:
        message_id = deterministic_message_id(account_address, source_key)
    message["Message-ID"] = message_id
    if original and action in {"message.reply", "message.reply_all"}:
        original_id = original.get("message_id")
        if original_id:
            message["In-Reply-To"] = original_id
            references = list(original.get("references", [])) + [original_id]
            message["References"] = " ".join(dict.fromkeys(references))
    custom_headers = payload.get("headers", {})
    if custom_headers:
        if not composition_config["allow_custom_headers"]:
            raise ContractError("custom email headers are disabled")
        custom_headers = require_object(custom_headers, "email custom headers")
        protected = {value.lower() for value in composition_config["protected_headers"]}
        for name, value in custom_headers.items():
            if not re.fullmatch(r"[A-Za-z0-9-]{1,128}", name) or name.lower() in protected:
                raise ContractError(f"custom email header {name!r} is forbidden")
            message[name] = _clean_header(value, f"email custom header {name}", outbound_config["max_subject_chars"])
    message.set_content(text_body or "")
    if html_body is not None:
        message.add_alternative(html_body, subtype="html")
    attachment_evidence: list[dict[str, Any]] = []
    attachments = payload.get("attachments", [])
    if not isinstance(attachments, list):
        raise ContractError("attachments must be an array")
    attachments = list(attachments)
    include_attachments = payload.get("include_attachments", False)
    if not isinstance(include_attachments, bool):
        raise ContractError("include_attachments must be a boolean")
    if include_attachments and original is not None:
        for item in original.get("attachments", []):
            attachments.append(
                {
                    "path": item["stored_path"],
                    "filename": item["filename"],
                    "media_type": item["media_type"],
                    "disposition": item["content_disposition"],
                    "content_id": item.get("content_id"),
                    "sha256": item["sha256"],
                    "size": item["size"],
                }
            )
    if len(attachments) > outbound_config["max_attachments"]:
        raise ContractError("outbound email exceeds max_attachments")
    total_attachment_bytes = 0
    for index, item in enumerate(attachments):
        resolved = _attachment_from_payload(item, resolver, index, outbound_config["max_attachment_bytes"])
        total_attachment_bytes += len(resolved.data)
        if total_attachment_bytes > outbound_config["max_total_attachment_bytes"]:
            raise ContractError("outbound email exceeds max_total_attachment_bytes")
        maintype, subtype = resolved.media_type.split("/", 1)
        kwargs: dict[str, Any] = {
            "maintype": maintype,
            "subtype": subtype,
            "filename": resolved.filename,
            "disposition": resolved.disposition,
        }
        if resolved.content_id:
            kwargs["cid"] = resolved.content_id
        message.add_attachment(resolved.data, **kwargs)
        attachment_evidence.append(
            {
                "filename": resolved.filename,
                "media_type": resolved.media_type,
                "size": len(resolved.data),
                "sha256": hashlib.sha256(resolved.data).hexdigest(),
                "source": resolved.source,
            }
        )
    if action == "message.forward" and original and payload.get("attach_original", False):
        raw_original = original.get("raw_bytes")
        if not isinstance(raw_original, bytes):
            raise ContractError("forward attach_original requires the original raw message")
        message.add_attachment(raw_original, maintype="message", subtype="rfc822", filename="forwarded-message.eml")
    serialized = message.as_bytes(policy=policy.SMTP)
    if len(serialized) > outbound_config["max_message_bytes"]:
        raise ContractError("outbound MIME message exceeds max_message_bytes")
    return message, [item["address"] for item in envelope_recipients], attachment_evidence


def validate_sha256(value: Any, name: str) -> str:
    text = require_string(value, name)
    if not _HEX64_RE.fullmatch(text):
        raise ContractError(f"{name} must be a lowercase SHA-256 digest")
    return text


def encode_inline_bytes(data: bytes, limit: int) -> str | None:
    if len(data) > limit:
        return None
    return base64.b64encode(data).decode("ascii")
