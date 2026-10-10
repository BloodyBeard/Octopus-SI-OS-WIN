import hashlib
import json
import os
import re
import urllib.parse


OPERATION_IDS = frozenset(
    {
        "cloud_work.capabilities.status",
        "cloud_work.environment.list",
        "cloud_work.task.create",
        "cloud_work.task.list",
        "cloud_work.task.status",
        "cloud_work.task.attempts",
        "cloud_work.task.result",
        "cloud_work.task.diff",
        "cloud_work.task.apply_candidate",
    }
)
OPERATION_OUTPUT_LABELS = {
    "cloud_work.capabilities.status": "cloud_work.status",
    "cloud_work.environment.list": "cloud_work.result",
    "cloud_work.task.create": "cloud_work.status",
    "cloud_work.task.list": "cloud_work.status",
    "cloud_work.task.status": "cloud_work.status",
    "cloud_work.task.attempts": "cloud_work.attempt",
    "cloud_work.task.result": "cloud_work.result",
    "cloud_work.task.diff": "cloud_work.diff",
    "cloud_work.task.apply_candidate": "cloud_work.apply_candidate",
}
TASK_STATUSES = frozenset({"pending", "ready", "applied", "error"})
ATTEMPT_STATUSES = frozenset(
    {"pending", "in_progress", "completed", "failed", "cancelled", "unknown"}
)
_HEX64 = re.compile(r"^[0-9a-f]{64}$")
_GIT_COMMIT = re.compile(r"^[0-9a-fA-F]{40}(?:[0-9a-fA-F]{24})?$")


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


def validate_config(value):
    value = require_keys(value, {"schema_version", "transport", "contract", "driver_ipc"}, set(), "cloud work config")
    if value["schema_version"] != 1:
        raise ContractError("cloud work config schema_version must be 1")
    transport = require_keys(
        value["transport"],
        {
            "base_url_env",
            "auth_token_env",
            "account_id_env",
            "request_timeout_ms",
            "max_response_bytes",
            "user_agent",
        },
        set(),
        "cloud work transport",
    )
    for key in ("base_url_env", "auth_token_env", "account_id_env"):
        env_name = require_string(transport[key], f"cloud work transport.{key}", 128)
        if not re.fullmatch(r"[A-Z][A-Z0-9_]*", env_name):
            raise ContractError(f"cloud work transport.{key} is not an environment variable name")
    require_int(transport["request_timeout_ms"], "cloud work transport.request_timeout_ms", 1000, 600000)
    require_int(transport["max_response_bytes"], "cloud work transport.max_response_bytes", 1024, 268435456)
    require_string(transport["user_agent"], "cloud work transport.user_agent", 256)
    contract = require_keys(
        value["contract"],
        {
            "accepted_labels",
            "max_prompt_chars",
            "max_task_id_chars",
            "max_cursor_chars",
            "max_list_limit",
            "max_attempts",
            "max_diff_bytes",
            "operation_catalog_dir",
        },
        set(),
        "cloud work contract",
    )
    if contract["accepted_labels"] != ["cloud_work.request"]:
        raise ContractError("cloud work contract accepts only cloud_work.request")
    require_int(contract["max_prompt_chars"], "cloud work contract.max_prompt_chars", 1, 1000000)
    require_int(contract["max_task_id_chars"], "cloud work contract.max_task_id_chars", 1, 4096)
    require_int(contract["max_cursor_chars"], "cloud work contract.max_cursor_chars", 1, 16384)
    require_int(contract["max_list_limit"], "cloud work contract.max_list_limit", 1, 100)
    require_int(contract["max_attempts"], "cloud work contract.max_attempts", 1, 16)
    require_int(contract["max_diff_bytes"], "cloud work contract.max_diff_bytes", 1, 268435456)
    catalog_dir = require_string(contract["operation_catalog_dir"], "cloud work contract.operation_catalog_dir", 128)
    if os.path.basename(catalog_dir) != catalog_dir:
        raise ContractError("cloud work operation catalog must be interface-relative")
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
        "cloud work driver_ipc",
    )
    require_int(ipc["ledger_batch_size"], "cloud work driver_ipc.ledger_batch_size", 1, 1024)
    require_int(ipc["idle_sleep_ms"], "cloud work driver_ipc.idle_sleep_ms", 10, 60000)
    require_int(ipc["verdict_timeout_ms"], "cloud work driver_ipc.verdict_timeout_ms", 1000, 600000)
    for key in ("max_cached_manifest_bytes", "max_outbound_payload_bytes", "ledger_fallback_max_line_bytes"):
        require_int(ipc[key], f"cloud work driver_ipc.{key}", 1024, 536870912)
    return value


def load_config(path):
    return validate_config(read_json(path, "cloud work config"))


def validate_activation(value, interface_id="world_cloud_work"):
    value = require_keys(value, {"schema_version", "interface", "activations"}, set(), "cloud work activation")
    if value["schema_version"] != 1 or value["interface"] != interface_id:
        raise ContractError("cloud work activation identity is invalid")
    activations = value["activations"]
    if not isinstance(activations, list) or len(activations) != 1:
        raise ContractError("cloud work activation must declare one activation")
    activation = require_keys(activations[0], {"enabled", "activation_driver_ref", "config"}, set(), "cloud work activation entry")
    require_bool(activation["enabled"], "cloud work activation enabled")
    if activation["activation_driver_ref"] != "app_server_transport":
        raise ContractError("cloud work activation must use app_server_transport")
    config = require_keys(activation["config"], {"bind_interface_socket_when_env_unset"}, set(), "cloud work activation config")
    if config["bind_interface_socket_when_env_unset"] is not True:
        raise ContractError("cloud work activation must bind its interface socket")
    return value


def load_operation_catalog(interface_dir):
    operations_dir = os.path.join(interface_dir, "operations")
    found = {}
    for name in sorted(os.listdir(operations_dir)):
        if not name.endswith(".json"):
            raise ContractError(f"cloud work operation catalog contains unexpected entry {name!r}")
        value = read_json(os.path.join(operations_dir, name), f"cloud work operation {name}")
        value = require_keys(
            value,
            {"id", "request_label", "output_label", "handler_config", "description", "input_schema"},
            set(),
            f"cloud work operation {name}",
        )
        identifier = require_string(value["id"], f"cloud work operation {name}.id", 256)
        if identifier in found:
            raise ContractError(f"cloud work operation id {identifier!r} is duplicated")
        if value["request_label"] != "cloud_work.request":
            raise ContractError(f"cloud work operation {identifier} request label drifted")
        if value["output_label"] != OPERATION_OUTPUT_LABELS.get(identifier):
            raise ContractError(f"cloud work operation {identifier} output label drifted")
        handler = require_keys(value["handler_config"], {"operation"}, set(), f"cloud work operation {identifier}.handler_config")
        operation_name = require_string(handler["operation"], f"cloud work operation {identifier}.handler_config.operation", 128)
        if operation_name != identifier.removeprefix("cloud_work."):
            raise ContractError(f"cloud work operation {identifier} handler operation drifted")
        require_object(value["input_schema"], f"cloud work operation {identifier}.input_schema")
        found[identifier] = value
    if set(found) != set(OPERATION_IDS):
        missing = sorted(set(OPERATION_IDS) - set(found))
        extra = sorted(set(found) - set(OPERATION_IDS))
        raise ContractError(f"cloud work operation catalog drifted; missing={missing}, extra={extra}")
    return found


def normalize_task_id(value, maximum=512):
    value = require_string(value, "cloud work task_id", maximum)
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme or parsed.netloc:
        segments = [urllib.parse.unquote(item) for item in parsed.path.split("/") if item]
        candidates = [segments[index + 1] for index, item in enumerate(segments[:-1]) if item == "tasks"]
        if not candidates:
            raise ContractError("cloud work task URL has no tasks/<id> segment")
        value = candidates[-1]
    value = value.split("?", 1)[0].split("#", 1)[0]
    return require_string(value, "cloud work task_id", maximum)


def parse_repository_remote(value):
    value = require_string(value, "cloud work repository_remote", 4096)
    match = re.fullmatch(r"(?:[^@/:]+@)?github\.com:(?P<owner>[^/]+)/(?P<repo>[^/]+)", value)
    if match is None:
        match = re.fullmatch(
            r"(?:(?:https?|git|ssh)://)?(?:[^@/]+@)?github\.com/(?P<owner>[^/]+)/(?P<repo>[^/]+)",
            value,
        )
    if match is None:
        raise ContractError("cloud work repository_remote must identify a GitHub owner/repository")
    owner = match.group("owner").strip()
    repo = match.group("repo").strip()
    if repo.endswith(".git"):
        repo = repo[:-4]
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", owner) or not re.fullmatch(r"[A-Za-z0-9_.-]+", repo):
        raise ContractError("cloud work repository_remote contains unsafe owner/repository text")
    return {"provider": "github", "owner": owner, "repository": repo, "sanitized": f"https://github.com/{owner}/{repo}"}


def _optional_string(value, key, maximum):
    if key not in value:
        return None
    return require_string(value[key], f"cloud work arguments.{key}", maximum)


def _validate_task_selector(arguments, config):
    task_id = normalize_task_id(arguments["task_id"], config["contract"]["max_task_id_chars"])
    turn_id = _optional_string(arguments, "turn_id", 512)
    placement = arguments.get("attempt_placement")
    if placement is not None:
        placement = require_int(
            placement,
            "cloud work arguments.attempt_placement",
            0,
            config["contract"]["max_attempts"] - 1,
        )
    if turn_id is not None and placement is not None:
        raise ContractError("cloud work attempt selector accepts turn_id or attempt_placement, not both")
    return task_id, turn_id, placement


def validate_request(value, config):
    value = require_keys(value, {"schema_version", "request_id", "operation", "arguments"}, set(), "cloud work request")
    if value["schema_version"] != 1:
        raise ContractError("cloud work request schema_version must be 1")
    request_id = require_string(value["request_id"], "cloud work request.request_id", 256)
    operation = require_string(value["operation"], "cloud work request.operation", 256)
    if operation not in OPERATION_IDS:
        raise ContractError(f"cloud work operation {operation!r} is unsupported")
    arguments = require_object(value["arguments"], "cloud work request.arguments")
    if operation == "cloud_work.capabilities.status":
        require_keys(arguments, set(), set(), "cloud work capabilities arguments")
    elif operation == "cloud_work.environment.list":
        require_keys(arguments, set(), {"repository_remote"}, "cloud work environment.list arguments")
        if "repository_remote" in arguments:
            parse_repository_remote(arguments["repository_remote"])
    elif operation == "cloud_work.task.create":
        arguments = require_keys(
            arguments,
            {"environment_id", "prompt", "git_ref"},
            {"attempts", "qa_mode", "starting_diff"},
            "cloud work task.create arguments",
        )
        require_string(arguments["environment_id"], "cloud work arguments.environment_id", 512)
        require_string(arguments["prompt"], "cloud work arguments.prompt", config["contract"]["max_prompt_chars"])
        require_string(arguments["git_ref"], "cloud work arguments.git_ref", 1024)
        require_int(arguments.get("attempts", 1), "cloud work arguments.attempts", 1, config["contract"]["max_attempts"])
        require_bool(arguments.get("qa_mode", False), "cloud work arguments.qa_mode")
        if "starting_diff" in arguments:
            starting = require_keys(arguments["starting_diff"], {"artifact_ref", "sha256", "unified_diff"}, set(), "cloud work starting_diff")
            require_string(starting["artifact_ref"], "cloud work starting_diff.artifact_ref", 4096)
            digest = require_string(starting["sha256"], "cloud work starting_diff.sha256", 64)
            if not _HEX64.fullmatch(digest):
                raise ContractError("cloud work starting_diff.sha256 must be lowercase SHA-256")
            diff = starting["unified_diff"]
            require_unified_diff(diff)
            raw = diff.encode("utf-8")
            if len(raw) > config["contract"]["max_diff_bytes"]:
                raise ContractError("cloud work starting diff exceeds its byte bound")
            if hashlib.sha256(raw).hexdigest() != digest:
                raise ContractError("cloud work starting diff SHA-256 does not match its bytes")
    elif operation == "cloud_work.task.list":
        arguments = require_keys(arguments, set(), {"environment_id", "limit", "cursor", "include_review"}, "cloud work task.list arguments")
        _optional_string(arguments, "environment_id", 512)
        _optional_string(arguments, "cursor", config["contract"]["max_cursor_chars"])
        require_int(arguments.get("limit", config["contract"]["max_list_limit"]), "cloud work arguments.limit", 1, config["contract"]["max_list_limit"])
        require_bool(arguments.get("include_review", False), "cloud work arguments.include_review")
    elif operation == "cloud_work.task.status":
        require_keys(arguments, {"task_id"}, set(), "cloud work task.status arguments")
        normalize_task_id(arguments["task_id"], config["contract"]["max_task_id_chars"])
    elif operation in {"cloud_work.task.attempts", "cloud_work.task.result", "cloud_work.task.diff"}:
        allowed = {"turn_id"} if operation == "cloud_work.task.attempts" else {"turn_id", "attempt_placement"}
        require_keys(arguments, {"task_id"}, allowed, f"cloud work {operation} arguments")
        _validate_task_selector(arguments, config)
    elif operation == "cloud_work.task.apply_candidate":
        require_keys(arguments, {"task_id", "target"}, {"turn_id", "attempt_placement"}, "cloud work task.apply_candidate arguments")
        _validate_task_selector(arguments, config)
        target = require_keys(
            arguments["target"],
            {"root_slot", "repository_identity", "branch", "base_ref", "head_commit"},
            set(),
            "cloud work apply target",
        )
        if target["root_slot"] != "worktree_root":
            raise ContractError("cloud work apply target root_slot must be worktree_root")
        require_string(target["repository_identity"], "cloud work apply target.repository_identity", 4096)
        require_string(target["branch"], "cloud work apply target.branch", 1024)
        require_string(target["base_ref"], "cloud work apply target.base_ref", 1024)
        commit = require_string(target["head_commit"], "cloud work apply target.head_commit", 64)
        if not _GIT_COMMIT.fullmatch(commit):
            raise ContractError("cloud work apply target.head_commit must be a 40- or 64-character hexadecimal object id")
    return {"schema_version": 1, "request_id": request_id, "operation": operation, "arguments": arguments}


def require_unified_diff(value):
    if not isinstance(value, str) or not value or "\x00" in value:
        raise ContractError("cloud work unified diff must be a non-empty string without NUL")
    normalized = value.replace("\r\n", "\n").replace("\r", "\n")
    starts_git_diff = normalized.lstrip().startswith("diff --git ")
    has_headers = "\n--- " in normalized and "\n+++ " in normalized
    has_hunk = "\n@@ " in normalized or normalized.startswith("@@ ")
    if not starts_git_diff and not (has_headers and has_hunk):
        raise ContractError("cloud work diff is not a unified git diff")
    return normalized


def build_create_body(arguments):
    input_items = [
        {
            "type": "message",
            "role": "user",
            "content": [{"content_type": "text", "text": arguments["prompt"]}],
        }
    ]
    if "starting_diff" in arguments:
        starting = arguments["starting_diff"]
        input_items.append(
            {
                "type": "pre_apply_patch",
                "output_diff": {
                    "diff": starting["unified_diff"],
                },
            }
        )
    body = {
        "new_task": {
            "environment_id": arguments["environment_id"],
            "branch": arguments["git_ref"],
            "run_environment_in_qa_mode": arguments.get("qa_mode", False),
        },
        "input_items": input_items,
    }
    attempts = arguments.get("attempts", 1)
    if attempts > 1:
        body["metadata"] = {"best_of_n": attempts}
    return body


def _dig(value, *path):
    for item in path:
        if not isinstance(value, dict):
            return None
        value = value.get(item)
    return value


def _status_display(value):
    if not isinstance(value, dict):
        return {}
    task = value.get("task") if isinstance(value.get("task"), dict) else {}
    display = value.get("task_status_display", task.get("task_status_display"))
    return display if isinstance(display, dict) else {}


def normalize_task_status(value):
    display = _status_display(value)
    raw = _dig(display, "latest_turn_status_display", "turn_status")
    if not isinstance(raw, str):
        raw = _dig(value, "current_assistant_turn", "turn_status")
    if not isinstance(raw, str):
        state = display.get("state")
        if state in TASK_STATUSES:
            return state
    mapping = {
        "pending": "pending",
        "in_progress": "pending",
        "completed": "ready",
        "failed": "error",
        "cancelled": "error",
        "applied": "applied",
    }
    return mapping.get(raw, "pending")


def normalize_attempt_status(value):
    if not isinstance(value, str):
        return "unknown"
    mapped = {
        "pending": "pending",
        "in_progress": "in_progress",
        "completed": "completed",
        "failed": "failed",
        "cancelled": "cancelled",
    }.get(value, "unknown")
    if mapped not in ATTEMPT_STATUSES:
        raise ContractError("cloud work attempt status normalization failed")
    return mapped


def diff_summary(diff):
    if not isinstance(diff, str) or not diff:
        return {"files_changed": 0, "lines_added": 0, "lines_removed": 0}
    files = set()
    added = 0
    removed = 0
    for line in diff.replace("\r\n", "\n").replace("\r", "\n").split("\n"):
        if line.startswith("diff --git "):
            files.add(line)
        elif line.startswith("+") and not line.startswith("+++"):
            added += 1
        elif line.startswith("-") and not line.startswith("---"):
            removed += 1
    file_count = len(files)
    if file_count == 0 and diff.strip():
        file_count = 1
    return {"files_changed": file_count, "lines_added": added, "lines_removed": removed}


def extract_diff(value):
    candidates = [value.get("unified_diff") if isinstance(value, dict) else None]
    for name in ("current_diff_task_turn", "current_assistant_turn"):
        for current in (_dig(value, name), _dig(value, "task", name)):
            if not isinstance(current, dict):
                continue
            candidates.append(_dig(current, "output_diff", "diff"))
            for item in current.get("output_items", []) if isinstance(current.get("output_items"), list) else []:
                if not isinstance(item, dict):
                    continue
                if item.get("type") == "output_diff" and isinstance(item.get("diff"), str):
                    candidates.append(item["diff"])
                if item.get("type") == "pr":
                    candidates.append(_dig(item, "output_diff", "diff"))
    for candidate in candidates:
        if isinstance(candidate, str) and candidate:
            return require_unified_diff(candidate)
    return None


def extract_messages(value):
    result = []
    sources = []
    for name in ("current_diff_task_turn", "current_assistant_turn"):
        current = _dig(value, name)
        if isinstance(current, dict):
            sources.extend(current.get("output_items", []) if isinstance(current.get("output_items"), list) else [])
            sources.extend(_dig(current, "worklog", "messages") or [])
    for item in sources:
        if not isinstance(item, dict):
            continue
        role = _dig(item, "author", "role")
        if role not in {None, "assistant"} or item.get("type") not in {None, "message"}:
            continue
        content = item.get("content")
        if isinstance(content, dict):
            content = content.get("parts")
        if not isinstance(content, list):
            continue
        for part in content:
            if isinstance(part, str) and part:
                result.append(part)
            elif isinstance(part, dict) and part.get("content_type") == "text" and isinstance(part.get("text"), str) and part["text"]:
                result.append(part["text"])
    error = _dig(value, "current_assistant_turn", "error")
    if not result and isinstance(error, str) and error:
        result.append(f"Task failed: {error}")
    if not result and isinstance(error, dict):
        code = error.get("code") if isinstance(error.get("code"), str) else ""
        message = error.get("message") if isinstance(error.get("message"), str) else ""
        summary = ": ".join(item for item in (code, message) if item)
        if summary:
            result.append(f"Task failed: {summary}")
    return result


def extract_prompt(value):
    for turn in (_dig(value, "current_user_turn"), value):
        if not isinstance(turn, dict):
            continue
        parts = []
        for item in turn.get("input_items", []) if isinstance(turn.get("input_items"), list) else []:
            if not isinstance(item, dict) or item.get("type") != "message" or item.get("role", "user").lower() != "user":
                continue
            for part in item.get("content", []) if isinstance(item.get("content"), list) else []:
                if isinstance(part, str) and part.strip():
                    parts.append(part)
                elif isinstance(part, dict) and str(part.get("content_type", "")).lower() == "text" and isinstance(part.get("text"), str) and part["text"]:
                    parts.append(part["text"])
        if parts:
            return "\n\n".join(parts)
    for candidate in (_dig(value, "task", "prompt"), value.get("prompt") if isinstance(value, dict) else None):
        if isinstance(candidate, str):
            return candidate
    return ""


def normalize_task_summary(value):
    value = require_object(value, "cloud work task response")
    task = value.get("task") if isinstance(value.get("task"), dict) else value
    task_id = task.get("id")
    if not isinstance(task_id, str) or not task_id:
        raise ContractError("cloud work task response has no task id")
    diff = extract_diff(value)
    display = _status_display(value)
    latest = _dig(display, "latest_turn_status_display")
    latest = latest if isinstance(latest, dict) else {}
    stats = latest.get("diff_stats") if isinstance(latest.get("diff_stats"), dict) else {}
    summary = {
        "files_changed": stats.get("files_modified", latest.get("files_changed", latest.get("file_count", 0))),
        "lines_added": stats.get("lines_added", latest.get("lines_added", latest.get("additions", 0))),
        "lines_removed": stats.get("lines_removed", latest.get("lines_removed", latest.get("deletions", 0))),
    }
    if any(isinstance(item, bool) or not isinstance(item, int) or item < 0 for item in summary.values()):
        summary = diff_summary(diff)
    if summary == {"files_changed": 0, "lines_added": 0, "lines_removed": 0} and diff:
        summary = diff_summary(diff)
    current = value.get("current_assistant_turn") if isinstance(value.get("current_assistant_turn"), dict) else {}
    sibling_turn_ids = latest.get("sibling_turn_ids")
    attempt_total = len(sibling_turn_ids) + 1 if isinstance(sibling_turn_ids, list) else display.get("attempt_total", latest.get("attempt_total", 1))
    return {
        "task_id": task_id,
        "title": task.get("title") if isinstance(task.get("title"), str) else "<untitled>",
        "status": normalize_task_status(value),
        "updated_at": task.get("updated_at", task.get("created_at")),
        "environment_id": task.get("environment_id"),
        "environment_label": display.get("environment_label"),
        "diff_summary": summary,
        "is_review": bool(task.get("is_review") or task.get("pull_requests")),
        "attempt_total": attempt_total,
        "current_attempt": {
            "turn_id": current.get("id"),
            "placement": current.get("attempt_placement"),
            "status": normalize_attempt_status(current.get("turn_status")),
        },
    }


def normalize_task_list(value, include_review=False):
    value = require_object(value, "cloud work task list response")
    items = value.get("items", value.get("tasks"))
    if not isinstance(items, list):
        raise ContractError("cloud work task list response has no items array")
    tasks = []
    for item in items:
        if not isinstance(item, dict):
            raise ContractError("cloud work task list contains a non-object item")
        wrapped = item if "task" in item else {"task": item, "task_status_display": item.get("task_status_display")}
        summary = normalize_task_summary(wrapped)
        if include_review or not summary["is_review"]:
            tasks.append(summary)
    cursor = value.get("cursor")
    if cursor is not None and not isinstance(cursor, str):
        raise ContractError("cloud work task list cursor is not a string")
    return {"schema_version": 1, "tasks": tasks, "next_cursor": cursor}


def _attempt_from_turn(turn):
    turn = require_object(turn, "cloud work attempt")
    turn_id = turn.get("id")
    if not isinstance(turn_id, str) or not turn_id:
        raise ContractError("cloud work attempt has no turn id")
    wrapped = {"current_assistant_turn": turn}
    placement = turn.get("attempt_placement")
    if placement is not None:
        placement = require_int(placement, "cloud work attempt placement", 0)
    created_at = turn.get("created_at")
    if created_at is not None and (
        isinstance(created_at, bool) or not isinstance(created_at, (int, float, str))
    ):
        raise ContractError("cloud work attempt created_at must be a number, string, or null")
    return {
        "turn_id": turn_id,
        "placement": placement,
        "created_at": created_at,
        "status": normalize_attempt_status(turn.get("turn_status")),
        "messages": extract_messages(wrapped),
        "diff": extract_diff(wrapped),
    }


def normalize_attempts(details, siblings=None):
    details = require_object(details, "cloud work task details")
    attempts = []
    current = details.get("current_assistant_turn")
    if isinstance(current, dict) and isinstance(current.get("id"), str):
        current_attempt = _attempt_from_turn(current)
        current_diff = extract_diff(details)
        current_messages = extract_messages(details)
        if current_attempt["diff"] is None and current_diff is not None:
            current_attempt["diff"] = current_diff
        if current_messages:
            current_attempt["messages"] = current_messages
        attempts.append(current_attempt)
    if siblings is not None:
        siblings = require_object(siblings, "cloud work sibling attempts")
        rows = siblings.get("sibling_turns")
        if not isinstance(rows, list):
            raise ContractError("cloud work sibling attempt response has no sibling_turns array")
        attempts.extend(_attempt_from_turn(item) for item in rows)
    unique = {}
    for attempt in attempts:
        unique[attempt["turn_id"]] = attempt
    attempts = list(unique.values())
    def sort_key(item):
        created_at = item["created_at"]
        if isinstance(created_at, (int, float)) and not isinstance(created_at, bool):
            created_key = (0, 0, float(created_at), "")
        elif isinstance(created_at, str):
            created_key = (0, 1, 0.0, created_at)
        else:
            created_key = (1, 0, 0.0, "")
        return (
            item["placement"] is None,
            item["placement"] if isinstance(item["placement"], int) else 0,
            *created_key,
            item["turn_id"],
        )

    attempts.sort(key=sort_key)
    return attempts


def select_attempt(details, attempts, turn_id=None, placement=None):
    if turn_id is None and placement is None:
        current = details.get("current_assistant_turn")
        if not isinstance(current, dict) or not isinstance(current.get("id"), str):
            raise ContractError("cloud work task has no current attempt")
        selected = _attempt_from_turn(current)
    else:
        selected = next(
            (
                item
                for item in attempts
                if (turn_id is not None and item["turn_id"] == turn_id)
                or (placement is not None and item["placement"] == placement)
            ),
            None,
        )
        if selected is None:
            raise ContractError("cloud work selected attempt does not exist")
    return selected


def build_apply_candidate(task_id, task_summary, attempt, target):
    diff = attempt.get("diff")
    if not isinstance(diff, str) or not diff:
        raise ContractError("cloud work selected attempt has no diff")
    diff = require_unified_diff(diff)
    diff_bytes = diff.encode("utf-8")
    diff_sha256 = hashlib.sha256(diff_bytes).hexdigest()
    source = {
        "task_id": task_id,
        "environment_id": task_summary.get("environment_id"),
        "attempt": {
            "turn_id": attempt.get("turn_id"),
            "placement": attempt.get("placement"),
            "status": attempt.get("status"),
        },
        "diff": {
            "ref": f"cloud-work://tasks/{urllib.parse.quote(task_id, safe='')}/turns/{urllib.parse.quote(str(attempt.get('turn_id') or 'current'), safe='')}/diff",
            "sha256": diff_sha256,
            "size_bytes": len(diff_bytes),
            "unified_diff": diff,
        },
    }
    identity_body = json.dumps({"source": source, "target": target}, sort_keys=True, separators=(",", ":")).encode("utf-8")
    candidate_id = f"cloud-apply-{hashlib.sha256(identity_body).hexdigest()}"
    review_request_id = f"{candidate_id[:52]}-review"
    return {
        "schema_version": 1,
        "candidate_id": candidate_id,
        "applied": False,
        "effect_status": "not_applied",
        "source": source,
        "target": dict(target),
        "review_handoff": {
            "schema_version": 1,
            "separately_bound_run_required": True,
            "worker_environment_kind": "git_worktree",
            "door_interface": "system_coder_env",
            "candidate_digest": diff_sha256,
            "review_request": {
                "artifact_label": "operator.approval.request",
                "request_id": review_request_id,
                "candidate_id": candidate_id,
                "diff_sha256": diff_sha256,
                "target": dict(target),
            },
            "required_verified_artifacts": [
                {
                    "mark": "operator.approval",
                    "artifact_label": "operator.approval.response",
                    "verifier": "operator_approval",
                    "params": {"request_id": review_request_id},
                }
            ],
            "effect_preconditions": [
                "the receiving run is separately bound to system_coder_env",
                "its formed grant admits git_worktree and the selected git tool",
                "the exact candidate digest and target evidence are reviewed",
                "operator.approval is admitted through operator_approval on the authored checkpoint edge",
                "the target head_commit and branch still match immediately before the effect",
            ],
            "effect_result_contract": {
                "source": "git_worktree result artifact",
                "must_include": [
                    "candidate_id",
                    "diff_sha256",
                    "target_branch",
                    "before_head",
                    "after_head",
                    "changed_paths",
                    "tool_result",
                ],
                "cloud_interface_may_report_apply_success": False,
            },
        },
    }
