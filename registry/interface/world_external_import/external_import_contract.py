import hashlib
import json
import os
import re
import stat
import time
from pathlib import Path


class ContractError(ValueError):
    pass


PRODUCTS = {
    "claude_code": {
        "config_dir": ".claude",
        "settings": ["settings.json", "settings.local.json"],
        "instruction_names": ["CLAUDE.md", ".claude/CLAUDE.md"],
        "skills_dirs": ["skills"],
        "commands_dir": "commands",
        "workers_dir": "agents",
        "hooks_file": None,
        "mcp_files": [".mcp.json", ".claude.json"],
    },
    "cursor": {
        "config_dir": ".cursor",
        "settings": ["cli-config.json", "cli.json"],
        "instruction_names": [".cursorrules"],
        "skills_dirs": ["skills", "skills-cursor"],
        "commands_dir": "commands",
        "workers_dir": "agents",
        "hooks_file": "hooks.json",
        "mcp_files": ["mcp.json"],
    },
}

ITEM_CLASSES = frozenset(
    {
        "configuration",
        "instructions",
        "skills",
        "plugins",
        "mcp",
        "worker_templates",
        "hooks",
        "commands",
        "memory",
        "history",
    }
)

SECRET_KEY = re.compile(
    r"(?:^|_)(?:access[_-]?token|api[_-]?key|auth(?:orization)?|bearer|cookie|credential|password|private[_-]?key|refresh[_-]?token|secret|session[_-]?token|token)(?:$|_)",
    re.IGNORECASE,
)
SECRET_FILE = re.compile(
    r"(?:^|[._-])(?:auth|credentials?|oauth|private[_-]?key|secrets?|token)(?:[._-]|$)",
    re.IGNORECASE,
)
ENV_PLACEHOLDER = re.compile(r"^\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-[^}]*)?\}$")
URL_CREDENTIALS = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*://[^/@\s]+:[^/@\s]+@")
STATIC_ASSIGNMENT = re.compile(
    r"(?:^|\s)([A-Za-z_][A-Za-z0-9_]*(?:TOKEN|SECRET|PASSWORD|KEY|CREDENTIAL)[A-Za-z0-9_]*)=([^\s]+)",
    re.IGNORECASE,
)
FRONTMATTER_DELIMITER = re.compile(r"^---\s*$", re.MULTILINE)
CLAUDE_HOOK_EVENTS = frozenset(
    {
        "PreToolUse",
        "PermissionRequest",
        "PostToolUse",
        "PreCompact",
        "PostCompact",
        "SessionStart",
        "SessionEnd",
        "UserPromptSubmit",
        "SubagentStart",
        "SubagentStop",
        "Stop",
        "Interrupt",
    }
)
CURSOR_HOOK_EVENTS = {
    "preToolUse": "PreToolUse",
    "postToolUse": "PostToolUse",
    "preCompact": "PreCompact",
    "postCompact": "PostCompact",
    "sessionStart": "SessionStart",
    "subagentStart": "SubagentStart",
    "subagentStop": "SubagentStop",
    "beforeSubmitPrompt": "UserPromptSubmit",
    "stop": "Stop",
}


def require_object(value, name):
    if not isinstance(value, dict):
        raise ContractError(f"{name} must be an object")
    return value


def require_string(value, name, maximum=4096):
    if not isinstance(value, str) or not value.strip():
        raise ContractError(f"{name} must be a non-empty string")
    if len(value) > maximum:
        raise ContractError(f"{name} exceeds its size bound")
    return value


def require_int(value, name, minimum=0, maximum=None):
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise ContractError(f"{name} must be an integer >= {minimum}")
    if maximum is not None and value > maximum:
        raise ContractError(f"{name} exceeds {maximum}")
    return value


def read_json(path, name):
    raw = _read_regular(path, 8 * 1024 * 1024, name)
    try:
        return _load_json(raw)
    except (UnicodeDecodeError, json.JSONDecodeError, ContractError) as error:
        raise ContractError(f"{name} is invalid JSON: {error}") from error


def validate_config(value):
    value = require_object(value, "external import config")
    if value.get("schema_version") != 1 or set(value) != {"schema_version", "contract", "driver_ipc"}:
        raise ContractError("external import config has an unsupported shape")
    contract = require_object(value["contract"], "external import contract config")
    if contract.get("operation") != "external_import.inspect":
        raise ContractError("external import operation is unsupported")
    if contract.get("source_root_slot") != "external_import_source_root":
        raise ContractError("external import source root slot is unsupported")
    for key in ("max_file_bytes", "max_total_bytes", "max_files", "max_session_age_days", "max_sessions", "max_text_chars"):
        require_int(contract.get(key), f"external import {key}", 1)
    if contract.get("products") != ["claude_code", "cursor"]:
        raise ContractError("external import product catalog drifted")
    if set(contract.get("classes", [])) != ITEM_CLASSES:
        raise ContractError("external import class catalog drifted")
    return value


def load_config(interface_dir):
    return validate_config(read_json(Path(interface_dir) / "config.json", "external import config"))


def validate_request(value, config):
    value = require_object(value, "external import request")
    required = {"schema_version", "request_id", "operation", "source"}
    optional = {"classes", "session_limits"}
    if set(value) - required - optional or required - set(value):
        raise ContractError("external import request has unknown or missing fields")
    if value["schema_version"] != 1 or value["operation"] != config["contract"]["operation"]:
        raise ContractError("external import request version or operation is unsupported")
    request_id = require_string(value["request_id"], "external import request_id", 256)
    source = require_object(value["source"], "external import source")
    if set(source) != {"product", "root_slot", "scope", "relative_root"}:
        raise ContractError("external import source has an unsupported shape")
    product = require_string(source["product"], "external import product", 64)
    if product not in config["contract"]["products"]:
        raise ContractError(f"external import product {product!r} is unsupported")
    if source["root_slot"] != config["contract"]["source_root_slot"]:
        raise ContractError("external import request may only use its declared source root slot")
    scope = require_string(source["scope"], "external import scope", 32)
    if scope not in config["contract"]["scopes"]:
        raise ContractError(f"external import scope {scope!r} is unsupported")
    relative_root = require_string(source["relative_root"], "external import relative_root", 2048)
    relative = Path(relative_root)
    if relative.is_absolute() or any(part in {"", ".", ".."} for part in relative.parts):
        if relative_root != ".":
            raise ContractError("external import relative_root must be a normalized relative path")
    classes = value.get("classes", list(config["contract"]["classes"]))
    if not isinstance(classes, list) or not classes or any(not isinstance(item, str) for item in classes):
        raise ContractError("external import classes must be a non-empty string array")
    if len(set(classes)) != len(classes) or any(item not in ITEM_CLASSES for item in classes):
        raise ContractError("external import classes contain duplicates or unsupported values")
    limits = value.get("session_limits", {})
    limits = require_object(limits, "external import session_limits")
    if set(limits) - {"max_age_days", "max_sessions"}:
        raise ContractError("external import session_limits contain unknown fields")
    max_age_days = require_int(
        limits.get("max_age_days", config["contract"]["max_session_age_days"]),
        "external import max_age_days",
        1,
        3650,
    )
    max_sessions = require_int(
        limits.get("max_sessions", config["contract"]["max_sessions"]),
        "external import max_sessions",
        1,
        1000,
    )
    return {
        "schema_version": 1,
        "request_id": request_id,
        "operation": value["operation"],
        "source": {
            "product": product,
            "root_slot": source["root_slot"],
            "scope": scope,
            "relative_root": relative_root,
        },
        "classes": classes,
        "session_limits": {"max_age_days": max_age_days, "max_sessions": max_sessions},
    }


def inspect_request(request, root_slots, config, observed_at_unix_ms=None):
    request = validate_request(request, config)
    root_slots = require_object(root_slots, "external import root slots")
    root_value = root_slots.get(request["source"]["root_slot"])
    root = Path(require_string(root_value, "external import configured source root", 32768))
    root = _canonical_directory(root, "external import configured source root")
    relative = request["source"]["relative_root"]
    scope_root = root if relative == "." else _within(root, root / relative, "external import scope root")
    scope_root = _canonical_directory(scope_root, "external import scope root")
    builder = CandidateBuilder(request, root, scope_root, config, observed_at_unix_ms)
    builder.inspect()
    return builder.bundle()


class CandidateBuilder:
    def __init__(self, request, configured_root, scope_root, config, observed_at_unix_ms):
        self.request = request
        self.configured_root = configured_root
        self.scope_root = scope_root
        self.config = config
        self.product = request["source"]["product"]
        self.scope = request["source"]["scope"]
        self.spec = PRODUCTS[self.product]
        self.config_root = scope_root / self.spec["config_dir"]
        self.candidates = []
        self.receipts = []
        self.files_seen = 0
        self.bytes_seen = 0
        self.observed_at_unix_ms = observed_at_unix_ms if observed_at_unix_ms is not None else int(time.time() * 1000)

    def inspect(self):
        operations = {
            "configuration": self._configuration,
            "instructions": self._instructions,
            "skills": self._skills,
            "plugins": self._plugins,
            "mcp": self._mcp,
            "worker_templates": self._worker_templates,
            "hooks": self._hooks,
            "commands": self._commands,
            "memory": self._memory,
            "history": self._history,
        }
        for item_class in self.request["classes"]:
            try:
                operations[item_class]()
            except (ContractError, OSError) as error:
                self._receipt(
                    item_class,
                    "error",
                    "inspection_failed",
                    str(error),
                    self._scope_provenance(),
                    [],
                )

    def bundle(self):
        counts = {status: sum(1 for item in self.receipts if item["status"] == status) for status in ("candidate", "refused", "unsupported", "error")}
        return {
            "schema_version": 1,
            "request_id": self.request["request_id"],
            "source": {
                **self.request["source"],
                "configured_root_fingerprint": hashlib.sha256(str(self.configured_root).encode()).hexdigest(),
            },
            "observed_at_unix_ms": self.observed_at_unix_ms,
            "facts": {
                "inspection_completed": True,
                "source_writes": 0,
                "registry_writes": 0,
                "policy_writes": 0,
                "run_writes": 0,
                "bindings_authored": 0,
                "secrets_copied": 0,
                "applications_installed": 0,
                "drivers_installed": 0,
                "live_state_claimed": False,
            },
            "counts": {**counts, "candidates": len(self.candidates), "receipts": len(self.receipts)},
            "candidates": self.candidates,
            "receipts": self.receipts,
        }

    def _configuration(self):
        settings_paths = [self.config_root / name for name in self.spec["settings"]]
        for path in settings_paths:
            if not _is_regular(path):
                continue
            value, provenance = self._json(path, "configuration")
            if not isinstance(value, dict):
                self._unsupported("configuration", path, "settings_root_not_object", "Settings root is not an object")
                continue
            self._settings_candidates(path, value, provenance)
        for sensitive in ("auth.json", "credentials.json", ".credentials.json", "oauth.json"):
            path = self.config_root / sensitive
            if _path_exists(path):
                self._refused("configuration", path, "private_auth_not_importable", "Private authentication state is never imported")

    def _settings_candidates(self, path, value, provenance):
        env = value.get("env")
        if isinstance(env, dict) and env:
            names = sorted(str(key) for key in env)
            self._candidate(
                "secret_handle_requirements_candidate",
                "configuration",
                f"Environment handles from {path.name}",
                {"required_names": names, "values_included": False, "binding_state": "not_authored"},
                provenance,
            )
            self._receipt("configuration", "refused", "private_values_redacted", f"Redacted {len(names)} environment values", provenance, [])
        provider_keys = ("model", "modelProvider", "model_provider", "apiBaseUrl", "baseUrl")
        provider = {key: value[key] for key in provider_keys if key in value and _safe_scalar(value[key])}
        if provider:
            self._candidate(
                "provider_interface_candidate",
                "configuration",
                f"Provider configuration from {path.name}",
                {"source_values": provider, "auth_included": False, "exact_interface_id": None},
                provenance,
            )
        worker_keys = ("effort", "reasoningEffort", "permissionMode", "approvalMode", "profile", "name")
        worker = {key: value[key] for key in worker_keys if key in value and _safe_scalar(value[key])}
        if worker:
            self._candidate(
                "named_worker_template_candidate",
                "configuration",
                f"Worker profile from {path.name}",
                {"display": worker, "worker_pin_authored": False, "run_created": False},
                provenance,
            )
        sandbox = value.get("sandbox")
        if isinstance(sandbox, dict):
            safe_sandbox, removed = _redact_object(sandbox)
            self._candidate(
                "worker_environment_kind_candidate",
                "configuration",
                f"Local execution boundary from {path.name}",
                {"source_settings": safe_sandbox, "private_values_removed": removed, "driver_installed": False},
                provenance,
            )
        known = set(provider_keys) | set(worker_keys) | {"env", "sandbox", "hooks", "enabledPlugins", "extraKnownMarketplaces", "mcpServers", "enabledMcpjsonServers", "disabledMcpjsonServers", "disableAllHooks"}
        for key in sorted(set(value) - known):
            self._receipt("configuration", "unsupported", "unknown_setting", f"Unknown setting preserved as unsupported: {key}", provenance, [])

    def _instructions(self):
        for relative in self.spec["instruction_names"]:
            path = self.scope_root / relative
            if not _is_regular(path):
                continue
            text, provenance = self._text(path, "instructions")
            if text is None:
                continue
            self._candidate(
                "context_artifact_candidate",
                "instructions",
                f"Instructions from {relative}",
                {"label": "instructions.external", "text": text, "artifact_binding_authored": False},
                provenance,
            )

    def _skills(self):
        for directory in self.spec["skills_dirs"]:
            root = self.config_root / directory
            for skill_dir in self._directories(root, "skills"):
                members = self._text_tree(skill_dir, "skills")
                if not members:
                    self._unsupported("skills", skill_dir, "empty_or_binary_skill", "Skill has no importable text members")
                    continue
                provenance = _aggregate_provenance(
                    [member.pop("provenance") for member in members],
                    self.product,
                    self.scope,
                    self.request["source"]["root_slot"],
                )
                self._candidate(
                    "skill_artifact_candidate",
                    "skills",
                    f"Skill {skill_dir.name}",
                    {"name": skill_dir.name, "members": members, "artifact_binding_authored": False},
                    provenance,
                )

    def _commands(self):
        root = self.config_root / self.spec["commands_dir"]
        for path in self._files(root, {".md"}, recursive=True, item_class="commands"):
            text, provenance = self._text(path, "commands")
            if text is None:
                continue
            relative = path.relative_to(root).with_suffix("").as_posix()
            self._candidate(
                "skill_artifact_candidate",
                "commands",
                f"Command {relative}",
                {"name": relative, "source_form": "custom_command", "text": text, "slash_command_registry_created": False},
                provenance,
            )

    def _worker_templates(self):
        root = self.config_root / self.spec["workers_dir"]
        for path in self._files(root, {".md"}, recursive=False, item_class="worker_templates"):
            text, provenance = self._text(path, "worker_templates")
            if text is None:
                continue
            frontmatter, body, errors = _parse_frontmatter(text)
            if errors:
                self._receipt("worker_templates", "unsupported", "invalid_frontmatter", errors, provenance, [])
                continue
            name = frontmatter.get("name") or path.stem
            description = frontmatter.get("description")
            self._candidate(
                "named_worker_template_candidate",
                "worker_templates",
                f"Worker template {name}",
                {
                    "display_name": name,
                    "description": description,
                    "role_artifact": {"label": "worker.role.external", "text": body.strip()},
                    "source_hints": {key: frontmatter[key] for key in ("effort", "permissionMode", "model") if key in frontmatter},
                    "worker_pin_authored": False,
                    "run_created": False,
                },
                provenance,
            )

    def _hooks(self):
        paths = [self.config_root / name for name in self.spec["settings"]]
        if self.spec["hooks_file"]:
            paths.append(self.config_root / self.spec["hooks_file"])
        for path in paths:
            if not _is_regular(path):
                continue
            value, provenance = self._json(path, "hooks")
            hooks = value.get("hooks") if isinstance(value, dict) else None
            if not isinstance(hooks, dict):
                continue
            if value.get("disableAllHooks") is True:
                self._receipt("hooks", "unsupported", "source_hooks_disabled", "Source disables all hooks", provenance, [])
                continue
            for event, groups in sorted(hooks.items()):
                mapped_event = event if self.product == "claude_code" and event in CLAUDE_HOOK_EVENTS else CURSOR_HOOK_EVENTS.get(event)
                if mapped_event is None:
                    self._receipt("hooks", "unsupported", "hook_event_unsupported", f"Hook event {event} is unsupported", provenance, [])
                    continue
                if not isinstance(groups, list):
                    self._receipt("hooks", "unsupported", "hook_group_not_array", f"Hook event {event} is not an array", provenance, [])
                    continue
                for index, group in enumerate(groups):
                    self._hook_group(mapped_event, index, group, provenance, event)

    def _hook_group(self, event, index, group, provenance, source_event):
        if not isinstance(group, dict):
            self._receipt("hooks", "unsupported", "hook_group_not_object", f"Hook {event}[{index}] is not an object", provenance, [])
            return
        if "if" in group:
            self._receipt("hooks", "unsupported", "conditional_hook_unsupported", f"Hook {source_event}[{index}] is conditional", provenance, [])
            return
        entries = group.get("hooks")
        if not isinstance(entries, list):
            entries = [group]
        for entry_index, entry in enumerate(entries):
            if not isinstance(entry, dict):
                self._receipt("hooks", "unsupported", "hook_entry_not_object", f"Hook {event}[{index}].{entry_index} is not an object", provenance, [])
                continue
            command = entry.get("command")
            hook_type = entry.get("type", "command")
            if hook_type != "command" or not isinstance(command, str) or not command.strip():
                self._receipt("hooks", "unsupported", "hook_type_unsupported", f"Hook {event}[{index}].{entry_index} is not a supported command", provenance, [])
                continue
            if entry.get("async") is True or any(key in entry for key in ("asyncRewake", "shell", "once", "if")):
                self._receipt("hooks", "unsupported", "hook_execution_mode_unsupported", f"Hook {source_event}[{index}].{entry_index} has unsupported execution semantics", provenance, [])
                continue
            if _contains_secret_text(command):
                self._receipt("hooks", "refused", "hook_command_contains_private_value", f"Hook {event}[{index}].{entry_index} contains a private-looking value", provenance, [])
                continue
            self._candidate(
                "interface_behavior_candidate",
                "hooks",
                f"Hook {event}",
                {
                    "event": event,
                    "source_event": source_event,
                    "matcher": group.get("matcher") if _safe_scalar(group.get("matcher")) else None,
                    "command": command,
                    "timeout_seconds": entry.get("timeoutSec", entry.get("timeout")),
                    "blocking": not bool(entry.get("async", False)),
                    "interface_binding_authored": False,
                    "effect_enabled": False,
                },
                provenance,
            )

    def _mcp(self):
        paths = []
        for relative in self.spec["mcp_files"]:
            paths.append((self.config_root if self.product == "cursor" else self.scope_root) / relative)
        servers = {}
        provenances = {}
        for path in paths:
            if not _is_regular(path):
                continue
            value, provenance = self._json(path, "mcp")
            sources = [value]
            if self.product == "claude_code" and path.name == ".claude.json" and isinstance(value, dict):
                projects = value.get("projects")
                if isinstance(projects, dict):
                    for project_path, project_value in sorted(projects.items()):
                        if _same_directory(project_path, self.scope_root):
                            sources.append(project_value)
            for source in sources:
                source_servers = source.get("mcpServers") if isinstance(source, dict) else None
                if not isinstance(source_servers, dict):
                    continue
                for name, server in sorted(source_servers.items()):
                    servers[name] = server
                    provenances[name] = provenance
        enabled_names = set()
        disabled_names = set()
        for path in [self.config_root / name for name in self.spec["settings"]]:
            if not _is_regular(path):
                continue
            value, _ = self._json(path, "mcp")
            if not isinstance(value, dict):
                continue
            enabled_names.update(_string_array(value.get("enabledMcpjsonServers")))
            disabled_names.update(_string_array(value.get("disabledMcpjsonServers")))
        for name, server in sorted(servers.items()):
            source_enabled = name not in disabled_names and (not enabled_names or name in enabled_names)
            self._mcp_server(name, server, provenances[name], source_enabled)

    def _mcp_server(self, name, server, provenance, settings_enabled=True):
        if not isinstance(server, dict):
            self._receipt("mcp", "unsupported", "mcp_server_not_object", f"MCP server {name} is not an object", provenance, [])
            return
        source_enabled = settings_enabled and not (server.get("enabled") is False or server.get("disabled") is True)
        if isinstance(server.get("url"), str):
            url = server["url"]
            if _contains_secret_text(url) or URL_CREDENTIALS.match(url):
                self._receipt("mcp", "refused", "mcp_url_contains_private_value", f"MCP server {name} URL contains private authority", provenance, [])
                return
            header_names, env_headers, refused_headers = _header_requirements(server.get("headers"))
            if refused_headers:
                self._receipt("mcp", "refused", "static_mcp_headers_redacted", f"MCP server {name} static headers were redacted: {', '.join(refused_headers)}", provenance, [])
            self._candidate(
                "interface_application_candidate",
                "mcp",
                f"Remote capability {name}",
                {
                    "suggested_interface_id": _suggest_id("world_mcp", name),
                    "application_category": "plugin",
                    "transport": {"url": url, "header_names": header_names, "environment_header_requirements": env_headers},
                    "source_enabled": source_enabled,
                    "installed": False,
                    "project_available": False,
                    "task_connected": False,
                    "live": False,
                },
                provenance,
            )
            return
        if isinstance(server.get("command"), str):
            command = server["command"]
            args = server.get("args", [])
            if not isinstance(args, list) or any(not isinstance(item, str) for item in args):
                self._receipt("mcp", "unsupported", "local_mcp_args_invalid", f"Local MCP server {name} args are invalid", provenance, [])
                return
            if _contains_secret_text(command) or any(_contains_secret_text(item) for item in args):
                self._receipt("mcp", "refused", "local_mcp_command_contains_private_value", f"Local MCP server {name} command contains private-looking authority", provenance, [])
                return
            env_names, refused_env = _environment_requirements(server.get("env"))
            if refused_env:
                self._receipt("mcp", "refused", "static_mcp_environment_redacted", f"Local MCP server {name} static environment values were redacted: {', '.join(refused_env)}", provenance, [])
            self._candidate(
                "worker_environment_kind_candidate",
                "mcp",
                f"Local capability {name}",
                {
                    "suggested_kind_name": _suggest_id("external", name),
                    "product_category": "driver",
                    "process": {"command": command, "args": args, "environment_requirements": env_names},
                    "source_enabled": source_enabled,
                    "driver_installed": False,
                    "worker_consent_authored": False,
                    "room_materialized": False,
                },
                provenance,
            )
            return
        self._receipt("mcp", "unsupported", "mcp_transport_unsupported", f"MCP server {name} has no supported remote or local transport", provenance, [])

    def _plugins(self):
        for path in [self.config_root / name for name in self.spec["settings"]]:
            if not _is_regular(path):
                continue
            value, provenance = self._json(path, "plugins")
            enabled = value.get("enabledPlugins") if isinstance(value, dict) else None
            if isinstance(enabled, dict):
                for plugin_id, state in sorted(enabled.items()):
                    if not isinstance(state, bool):
                        self._receipt("plugins", "unsupported", "plugin_state_unsupported", f"Plugin {plugin_id} has a non-boolean source state", provenance, [])
                        continue
                    self._candidate(
                        "application_source_candidate",
                        "plugins",
                        f"Plugin {plugin_id}",
                        {"plugin_id": plugin_id, "source_enabled": state, "source_materialized": False, "installed": False, "task_connected": False},
                        provenance,
                    )
            extra = value.get("extraKnownMarketplaces") if isinstance(value, dict) else None
            if isinstance(extra, dict):
                for name, source in sorted(extra.items()):
                    safe, removed = _redact_object(source)
                    self._candidate(
                        "application_source_candidate",
                        "plugins",
                        f"Scoped plugin marketplace {name}",
                        {"marketplace": name, "source": safe, "private_values_removed": removed, "installed": False},
                        provenance,
                    )
        if self.product == "claude_code":
            path = self.config_root / "plugins" / "known_marketplaces.json"
            if _is_regular(path):
                value, provenance = self._json(path, "plugins")
                if isinstance(value, dict):
                    for name, source in sorted(value.items()):
                        safe, removed = _redact_object(source)
                        self._candidate(
                            "application_source_candidate",
                            "plugins",
                            f"Plugin marketplace {name}",
                            {"marketplace": name, "source": safe, "private_values_removed": removed, "installed": False},
                            provenance,
                        )
        else:
            marketplaces = self.config_root / "plugins" / "marketplaces"
            for path in self._files(marketplaces, {".json"}, recursive=True, item_class="plugins"):
                if path.name != "marketplace.json" or path.parent.name != ".cursor-plugin":
                    continue
                value, provenance = self._json(path, "plugins")
                safe, removed = _redact_object(value)
                self._candidate(
                    "application_source_candidate",
                    "plugins",
                    f"Cursor marketplace {safe.get('name', path.parent.parent.name) if isinstance(safe, dict) else path.parent.parent.name}",
                    {"manifest": safe, "private_values_removed": removed, "installed": False},
                    provenance,
                )

    def _memory(self):
        if self.product != "claude_code":
            self._receipt("memory", "unsupported", "source_memory_not_supported", "Cursor memory import is not declared by the source contract", self._scope_provenance(), [])
            return
        root = self.config_root / "projects"
        for path in self._files(root, {".md"}, recursive=True, item_class="memory"):
            if "memory" not in path.relative_to(root).parts:
                continue
            text, provenance = self._text(path, "memory")
            if text is None:
                continue
            project_key = path.relative_to(root).parts[0]
            self._candidate(
                "memory_artifact_candidate",
                "memory",
                f"Memory {path.name}",
                {
                    "label": "memory.external",
                    "project_key": project_key,
                    "relative_path": path.relative_to(root / project_key / "memory").as_posix(),
                    "text": text,
                    "artifact_binding_authored": False,
                    "memory_state_updated": False,
                },
                provenance,
            )

    def _history(self):
        root = self.config_root / "projects"
        candidates = []
        if self.product == "claude_code":
            for project in self._directories(root, "history"):
                candidates.extend(self._files(project, {".jsonl"}, recursive=False, item_class="history"))
        else:
            for project in self._directories(root, "history"):
                candidates.extend(self._files(project / "agent-transcripts", {".jsonl"}, recursive=True, ignored_dirs={"subagents"}, item_class="history"))
        cutoff = time.time() - self.request["session_limits"]["max_age_days"] * 86400
        recent = []
        for path in candidates:
            try:
                modified = path.stat().st_mtime
            except OSError:
                continue
            if modified >= cutoff:
                recent.append((modified, path))
        recent.sort(key=lambda item: (-item[0], item[1].as_posix()))
        for _, path in recent[: self.request["session_limits"]["max_sessions"]]:
            raw, provenance = self._bytes(path, "history")
            records, title, connectors, issues = _history_records(raw, self.product)
            for issue in issues:
                self._receipt("history", "unsupported", issue["reason"], f"History row {issue['index']} is unsupported", provenance, [])
            self._candidate(
                "history_artifact_candidate",
                "history",
                f"History {title or path.stem}",
                {
                    "label": "history.external",
                    "title": title,
                    "source_workspace_path_included": False,
                    "records": records,
                    "connector_evidence": connectors,
                    "run_created": False,
                    "session_identity_created": False,
                    "transcript_inheritance": False,
                },
                provenance,
            )

    def _files(self, root, suffixes, recursive, ignored_dirs=None, item_class="configuration"):
        ignored_dirs = ignored_dirs or set()
        if not _is_directory(root):
            return []
        result = []
        pending = [root]
        while pending:
            directory = pending.pop()
            try:
                entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
            except OSError as error:
                raise ContractError(f"cannot inspect {self._relative(directory)}: {error}") from error
            for entry in entries:
                if entry.is_symlink():
                    self._refused(item_class, Path(entry.path), "symlink_not_followed", "Symlink was not followed")
                    continue
                if entry.is_dir(follow_symlinks=False):
                    if recursive and entry.name not in ignored_dirs:
                        pending.append(Path(entry.path))
                    continue
                path = Path(entry.path)
                if entry.is_file(follow_symlinks=False) and path.suffix.lower() in suffixes:
                    result.append(path)
        result.sort()
        return result

    def _directories(self, root, item_class):
        if not _is_directory(root):
            return []
        result = []
        for entry in sorted(os.scandir(root), key=lambda item: item.name):
            if entry.is_symlink():
                self._refused(item_class, Path(entry.path), "symlink_not_followed", "Symlink was not followed")
            elif entry.is_dir(follow_symlinks=False):
                result.append(Path(entry.path))
        return result

    def _text_tree(self, root, item_class):
        members = []
        text_suffixes = {".md", ".json", ".toml", ".yaml", ".yml", ".txt", ".py", ".js", ".ts", ".sh"}
        for path in self._files(root, text_suffixes, recursive=True, item_class=item_class):
            if SECRET_FILE.search(path.name):
                self._refused(item_class, path, "private_file_not_importable", "Private-looking file was not read")
                continue
            text, provenance = self._text(path, item_class)
            if text is None:
                continue
            members.append({"path": path.relative_to(root).as_posix(), "text": text, "provenance": provenance})
        return members

    def _json(self, path, item_class):
        raw, provenance = self._bytes(path, item_class)
        try:
            return _load_json(raw), provenance
        except (UnicodeDecodeError, json.JSONDecodeError, ContractError) as error:
            raise ContractError(f"{self._relative(path)} is invalid JSON: {error}") from error

    def _text(self, path, item_class):
        raw, provenance = self._bytes(path, item_class)
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as error:
            raise ContractError(f"{self._relative(path)} is not UTF-8 text") from error
        if len(text) > self.config["contract"]["max_text_chars"]:
            raise ContractError(f"{self._relative(path)} exceeds the text character bound")
        if _contains_secret_text(text):
            self._refused(item_class, path, "text_contains_private_value", "Text containing a private-looking value was not imported")
            return None, provenance
        return text, provenance

    def _bytes(self, path, item_class):
        path = _within(self.configured_root, path, "external import source file")
        if SECRET_FILE.search(path.name):
            self._refused(item_class, path, "private_file_not_importable", "Private-looking file was not read")
            raise ContractError(f"private file is not importable: {self._relative(path)}")
        raw = _read_regular(path, self.config["contract"]["max_file_bytes"], self._relative(path))
        self.files_seen += 1
        self.bytes_seen += len(raw)
        if self.files_seen > self.config["contract"]["max_files"] or self.bytes_seen > self.config["contract"]["max_total_bytes"]:
            raise ContractError("external import inspection exceeded its aggregate bounds")
        return raw, self._provenance(path, raw)

    def _provenance(self, path, raw):
        return {
            "product": self.product,
            "scope": self.scope,
            "root_slot": self.request["source"]["root_slot"],
            "relative_path": self._relative(path),
            "sha256": hashlib.sha256(raw).hexdigest(),
            "size_bytes": len(raw),
        }

    def _relative(self, path):
        return Path(path).relative_to(self.configured_root).as_posix()

    def _candidate(self, candidate_type, item_class, title, payload, provenance):
        body = {
            "schema_version": 1,
            "candidate_type": candidate_type,
            "item_class": item_class,
            "title": title,
            "source_provenance": provenance,
            "state": {
                "candidate_only": True,
                "installed": False,
                "project_available": False,
                "task_connected": False,
                "enabled": False,
                "live": False,
            },
            "payload": payload,
        }
        digest = hashlib.sha256(_canonical_json(body)).hexdigest()
        body["candidate_id"] = f"external-import-{digest}"
        self.candidates.append(body)
        self._receipt(item_class, "candidate", "candidate_emitted", title, provenance, [body["candidate_id"]])

    def _refused(self, item_class, path, reason, message):
        provenance = self._path_provenance(path)
        self._receipt(item_class, "refused", reason, message, provenance, [])

    def _unsupported(self, item_class, path, reason, message):
        provenance = self._path_provenance(path)
        self._receipt(item_class, "unsupported", reason, message, provenance, [])

    def _path_provenance(self, path):
        try:
            relative = self._relative(path)
        except ValueError:
            relative = None
        return {
            "product": self.product,
            "scope": self.scope,
            "root_slot": self.request["source"]["root_slot"],
            "relative_path": relative,
            "content_read": False,
        }

    def _scope_provenance(self):
        return {
            "product": self.product,
            "scope": self.scope,
            "root_slot": self.request["source"]["root_slot"],
            "relative_path": self.request["source"]["relative_root"],
            "content_read": False,
        }

    def _receipt(self, item_class, status, reason, message, provenance, candidate_ids):
        body = {
            "item_class": item_class,
            "status": status,
            "reason": reason,
            "message": str(message)[:4096],
            "source_provenance": provenance,
            "candidate_ids": list(candidate_ids),
        }
        body["receipt_id"] = f"external-import-receipt-{hashlib.sha256(_canonical_json(body)).hexdigest()}"
        self.receipts.append(body)


def _canonical_json(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def _load_json(raw):
    def reject_constant(value):
        raise ContractError(f"non-finite JSON number {value!r} is unsupported")

    return json.loads(raw, parse_constant=reject_constant)


def _canonical_directory(path, name):
    try:
        result = path.resolve(strict=True)
    except OSError as error:
        raise ContractError(f"{name} is unavailable: {error}") from error
    if not result.is_dir():
        raise ContractError(f"{name} is not a directory")
    return result


def _within(root, path, name):
    root = Path(root).resolve(strict=True)
    try:
        resolved = Path(path).resolve(strict=True)
    except OSError as error:
        raise ContractError(f"{name} is unavailable: {error}") from error
    if not resolved.is_relative_to(root):
        raise ContractError(f"{name} escapes its configured root slot")
    return resolved


def _read_regular(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ContractError(f"{name} is not a stable regular file")
        if opened.st_size > maximum:
            raise ContractError(f"{name} exceeds its byte bound")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ContractError(f"{name} grew beyond its byte bound")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def _path_exists(path):
    try:
        os.lstat(path)
        return True
    except FileNotFoundError:
        return False


def _is_regular(path):
    try:
        return stat.S_ISREG(os.lstat(path).st_mode)
    except OSError:
        return False


def _is_directory(path):
    try:
        return stat.S_ISDIR(os.lstat(path).st_mode)
    except OSError:
        return False


def _same_directory(candidate, expected):
    if not isinstance(candidate, str):
        return False
    try:
        return Path(candidate).resolve(strict=True) == Path(expected).resolve(strict=True)
    except OSError:
        return False


def _string_array(value):
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, str)]


def _safe_scalar(value):
    return isinstance(value, (str, int, float, bool)) and not isinstance(value, str) or isinstance(value, str) and not _contains_secret_text(value)


def _contains_secret_text(value):
    if not isinstance(value, str):
        return False
    return bool(URL_CREDENTIALS.match(value) or STATIC_ASSIGNMENT.search(value) or re.search(r"(?i)(?:bearer|token|api[_-]?key|password|secret)\s*[:=]\s*[^$\s{][^\s]*", value))


def _redact_object(value, key_path=()):
    removed = []
    if isinstance(value, dict):
        result = {}
        for key, child in sorted(value.items(), key=lambda item: str(item[0])):
            path = key_path + (str(key),)
            if SECRET_KEY.search(str(key)):
                result[str(key)] = "[REDACTED_PRIVATE_VALUE]"
                removed.append(".".join(path))
            else:
                result[str(key)], nested = _redact_object(child, path)
                removed.extend(nested)
        return result, removed
    if isinstance(value, list):
        result = []
        for index, child in enumerate(value):
            child_value, nested = _redact_object(child, key_path + (str(index),))
            result.append(child_value)
            removed.extend(nested)
        return result, removed
    if isinstance(value, str) and _contains_secret_text(value):
        return "[REDACTED_PRIVATE_VALUE]", [".".join(key_path)]
    return value, removed


def _environment_requirements(value):
    if not isinstance(value, dict):
        return [], []
    names = []
    refused = []
    for key, child in sorted(value.items()):
        match = ENV_PLACEHOLDER.fullmatch(child) if isinstance(child, str) else None
        if match and match.group(1) == key:
            names.append(key)
        else:
            refused.append(key)
    return names, refused


def _header_requirements(value):
    if not isinstance(value, dict):
        return [], {}, []
    names = []
    env = {}
    refused = []
    for key, child in sorted(value.items()):
        names.append(key)
        if isinstance(child, str) and child.lower().startswith("bearer "):
            child = child[7:]
        match = ENV_PLACEHOLDER.fullmatch(child) if isinstance(child, str) else None
        if match:
            env[key] = match.group(1)
        else:
            refused.append(key)
    return names, env, refused


def _parse_frontmatter(text):
    if not text.startswith("---\n") and not text.startswith("---\r\n"):
        return {}, text, None
    lines = text.splitlines()
    try:
        end = lines[1:].index("---") + 1
    except ValueError:
        return {}, text, "frontmatter has no closing delimiter"
    values = {}
    for line in lines[1:end]:
        if not line.strip():
            continue
        if ":" not in line:
            return {}, text, "frontmatter contains a non-scalar row"
        key, value = line.split(":", 1)
        key = key.strip()
        value = value.strip().strip('"\'')
        if not key or not value or key in values:
            return {}, text, "frontmatter contains an empty or duplicate scalar"
        values[key] = value
    return values, "\n".join(lines[end + 1 :]), None


def _aggregate_provenance(items, product, scope, root_slot):
    rows = sorted((item["relative_path"], item["sha256"], item["size_bytes"]) for item in items)
    return {
        "product": product,
        "scope": scope,
        "root_slot": root_slot,
        "members": [{"relative_path": path, "sha256": digest, "size_bytes": size} for path, digest, size in rows],
        "sha256": hashlib.sha256(_canonical_json(rows)).hexdigest(),
        "size_bytes": sum(size for _, _, size in rows),
    }


def _suggest_id(prefix, name):
    normalized = re.sub(r"[^a-z0-9]+", "_", str(name).lower()).strip("_")
    return f"{prefix}_{normalized or 'candidate'}"


def _history_records(raw, product):
    records = []
    title = None
    connectors = set()
    issues = []
    for index, line in enumerate(raw.splitlines()):
        if not line.strip():
            continue
        try:
            value = _load_json(line)
        except (json.JSONDecodeError, ContractError):
            issue = {"index": index, "reason": "invalid_jsonl_record"}
            records.append({"index": index, "status": "unsupported", "reason": issue["reason"]})
            issues.append(issue)
            continue
        if not isinstance(value, dict):
            issue = {"index": index, "reason": "record_not_object"}
            records.append({"index": index, "status": "unsupported", "reason": issue["reason"]})
            issues.append(issue)
            continue
        safe, removed = _redact_object(value)
        if len(_canonical_json(safe)) > 262144:
            issue = {"index": index, "reason": "record_exceeds_bound"}
            records.append({"index": index, "status": "unsupported", "reason": issue["reason"]})
            issues.append(issue)
            continue
        records.append({"index": index, "value": safe, "private_values_removed": removed})
        if safe.get("type") in {"custom-title", "custom_title"} and isinstance(safe.get("title"), str):
            title = safe["title"]
        message = safe.get("message")
        if title is None and isinstance(message, dict):
            text = message.get("content")
            role = safe.get("type", safe.get("role"))
            if role in {"user", "human"} and isinstance(text, str) and text.strip():
                title = text.strip().splitlines()[0][:120]
        for connector in _connector_names(safe, product):
            connectors.add(connector)
    return records, title, sorted(connectors), issues


def _connector_names(value, product):
    serialized = json.dumps(value, ensure_ascii=False)
    names = set()
    if product == "cursor":
        for match in re.finditer(r'"(?:toolName|name)"\s*:\s*"(?:mcp__|mcp[_-])([^"_]+)', serialized, re.IGNORECASE):
            names.add(match.group(1))
    else:
        for match in re.finditer(r'"(?:serverName|name)"\s*:\s*"([^"]+)"', serialized):
            if "mcp" in serialized[max(0, match.start() - 200) : match.end() + 200].lower():
                names.add(match.group(1))
    return names
