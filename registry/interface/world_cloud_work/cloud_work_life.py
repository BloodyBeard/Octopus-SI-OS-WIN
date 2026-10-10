#!/usr/bin/env python3
import hashlib
import json
import os
import re
import socket
import sqlite3
import stat
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from cloud_work_contract import ContractError
from cloud_work_contract import OPERATION_OUTPUT_LABELS
from cloud_work_contract import build_apply_candidate
from cloud_work_contract import build_create_body
from cloud_work_contract import extract_prompt
from cloud_work_contract import load_config
from cloud_work_contract import load_operation_catalog
from cloud_work_contract import normalize_attempts
from cloud_work_contract import normalize_task_id
from cloud_work_contract import normalize_task_list
from cloud_work_contract import normalize_task_summary
from cloud_work_contract import parse_repository_remote
from cloud_work_contract import read_json
from cloud_work_contract import require_int
from cloud_work_contract import require_object
from cloud_work_contract import require_string
from cloud_work_contract import select_attempt
from cloud_work_contract import validate_activation
from cloud_work_contract import validate_request
from octopus_content_hash import octopus_content_hash


SAFE_OPERATIONS = frozenset(
    {
        "cloud_work.capabilities.status",
        "cloud_work.environment.list",
        "cloud_work.task.list",
        "cloud_work.task.status",
        "cloud_work.task.attempts",
        "cloud_work.task.result",
        "cloud_work.task.diff",
        "cloud_work.task.apply_candidate",
    }
)


def atomic_bytes(path, value, mode=0o600):
    parent = os.path.dirname(path)
    os.makedirs(parent, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".tmp-", dir=parent)
    try:
        os.fchmod(descriptor, mode)
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = -1
            handle.write(value)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def atomic_json(path, value):
    atomic_bytes(path, json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2).encode("utf-8"))


def immutable_bytes(path, value):
    try:
        with open(path, "rb") as handle:
            existing = handle.read()
    except FileNotFoundError:
        atomic_bytes(path, value)
        return
    if existing != value:
        raise RuntimeError(f"immutable cloud work evidence changed at {path}")


def read_bounded_regular(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ContractError(f"{name} is not a stable regular file")
        if opened.st_size > maximum:
            raise ContractError(f"{name} exceeds its size bound")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ContractError(f"{name} grew beyond its size bound")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


class RuntimeIO:
    def __init__(self, config):
        self.config = config
        self.interface_id = os.environ["OCTOPUS_INTERFACE_ID"]
        self.interface_dir = os.environ["OCTOPUS_INTERFACE_DIR"]
        self.var_dir = os.environ["OCTOPUS_VAR_DIR"]
        self.movements_path = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
        self.blobs_dir = os.environ["OCTOPUS_BLOBS_DIR"]
        self.index_path = os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]
        self.payload_dir = os.path.join(self.var_dir, "payloads")
        self.effect_dir = os.path.join(self.var_dir, "effects")
        self.cursor_path = os.path.join(self.var_dir, "ledger-cursor.json")
        for path in (self.var_dir, self.payload_dir, self.effect_dir):
            os.makedirs(path, exist_ok=True)
        self.stdout_lock = threading.Lock()
        self.verdict_condition = threading.Condition()
        self.verdicts = {}
        self.verdict_error = None
        self.next_request_id = 1
        self.index_warning = None

    def start(self):
        threading.Thread(target=self._verdict_reader, daemon=True).start()

    def _verdict_reader(self):
        try:
            for line in sys.stdin:
                message = require_object(json.loads(line), "cloud work runtime verdict")
                if message.get("kind") != "delivery_verdict":
                    raise RuntimeError("cloud work life received unexpected runtime input")
                request_id = require_int(message.get("request_id"), "cloud work runtime verdict request_id", 1)
                with self.verdict_condition:
                    self.verdicts[request_id] = require_object(message.get("verdict"), "cloud work delivery verdict")
                    self.verdict_condition.notify_all()
            raise RuntimeError("cloud work runtime verdict input closed")
        except Exception as error:
            with self.verdict_condition:
                self.verdict_error = error
                self.verdict_condition.notify_all()

    def ready(self):
        with self.stdout_lock:
            sys.stdout.write('{"kind":"ready"}\n')
            sys.stdout.flush()

    def hand_manifest(self, manifest_path):
        with self.verdict_condition:
            request_id = self.next_request_id
            self.next_request_id += 1
        frame = {
            "kind": "arrival",
            "request_id": request_id,
            "artifact_manifest_path": manifest_path,
            "evidence": [],
        }
        with self.stdout_lock:
            sys.stdout.write(json.dumps(frame, separators=(",", ":")) + "\n")
            sys.stdout.flush()
        deadline = time.monotonic() + self.config["driver_ipc"]["verdict_timeout_ms"] / 1000
        with self.verdict_condition:
            while request_id not in self.verdicts and self.verdict_error is None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError(f"cloud work runtime verdict timed out for {request_id}")
                self.verdict_condition.wait(remaining)
            if self.verdict_error is not None:
                raise RuntimeError(f"cloud work runtime verdict reader failed: {self.verdict_error}")
            verdict = self.verdicts.pop(request_id)
        admitted = verdict.get("admitted")
        if isinstance(admitted, dict):
            return require_int(admitted.get("seq"), "cloud work admitted movement seq", 0)
        raise RuntimeError(f"runtime refused cloud work artifact: {verdict!r}")

    def hand_json(self, label, value, key):
        payload = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
        payload_path = os.path.join(self.payload_dir, f"{key}.payload")
        manifest_path = os.path.join(self.payload_dir, f"{key}.artifact.json")
        immutable_bytes(payload_path, payload)
        immutable_bytes(
            manifest_path,
            json.dumps({"label": label, "payload_path": payload_path}, sort_keys=True, separators=(",", ":")).encode("utf-8"),
        )
        return self.hand_manifest(manifest_path)

    def load_cursor(self):
        try:
            value = read_json(self.cursor_path, "cloud work ledger cursor")
        except FileNotFoundError:
            return 0
        if value.get("schema_version") != 1 or set(value) != {"schema_version", "next_seq"}:
            raise ContractError("cloud work ledger cursor is invalid")
        return require_int(value["next_seq"], "cloud work ledger cursor next_seq", 0)

    def commit_cursor(self, next_seq):
        atomic_json(self.cursor_path, {"schema_version": 1, "next_seq": next_seq})

    def _index_rows(self, next_seq):
        uri = f"file:{urllib.parse.quote(os.path.abspath(self.index_path))}?mode=ro"
        connection = sqlite3.connect(uri, uri=True, timeout=0.25)
        try:
            schema = connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone()
            if schema != ("3",):
                raise RuntimeError("cloud work ledger index schema is unsupported")
            rows = connection.execute(
                "SELECT seq, artifact_content, runtime_run, runtime_checkpoint FROM movements "
                "WHERE runtime_interface = ? AND kind = 'InterfaceOutbound' "
                "AND from_kind = 'interface' AND to_kind = 'interface' AND to_id = ? AND seq >= ? "
                "ORDER BY seq ASC LIMIT ?",
                (self.interface_id, self.interface_id, next_seq, self.config["driver_ipc"]["ledger_batch_size"]),
            ).fetchall()
        finally:
            connection.close()
        result = []
        for seq, content, run, checkpoint in rows:
            seq = require_int(seq, "cloud work ledger index seq", 0)
            if not isinstance(content, str) or not re.fullmatch(r"[0-9a-f]{64}", content):
                raise RuntimeError(f"cloud work ledger index movement {seq} has no artifact hash")
            result.append((seq, content, require_string(run, "cloud work ledger run", 512), require_string(checkpoint, "cloud work ledger checkpoint", 512)))
        return result

    def _endpoint(self, value):
        if isinstance(value, dict) and set(value) == {"Interface"} and isinstance(value.get("Interface"), str):
            return value["Interface"]
        return None

    def _ledger_rows(self, next_seq):
        try:
            handle = open(self.movements_path, "rb")
        except FileNotFoundError:
            return []
        result = []
        maximum = self.config["driver_ipc"]["ledger_fallback_max_line_bytes"]
        with handle:
            for line in handle:
                if len(line) > maximum:
                    raise RuntimeError("cloud work ledger row exceeds its bound")
                movement = require_object(json.loads(line), "cloud work ledger movement")
                seq = require_int(movement.get("seq"), "cloud work ledger movement seq", 0)
                if seq < next_seq or movement.get("kind") != "InterfaceOutbound" or self._endpoint(movement.get("to")) != self.interface_id or self._endpoint(movement.get("from")) is None:
                    continue
                metadata = require_object(movement.get("invocation_metadata"), "cloud work movement metadata")
                runtime = require_object(metadata.get("runtime"), "cloud work movement runtime metadata")
                if runtime.get("interface") != self.interface_id:
                    raise ContractError("cloud work movement runtime target mismatched its exact interface")
                artifact = require_object(movement.get("artifact"), "cloud work movement artifact")
                content = artifact.get("content")
                if isinstance(content, list):
                    if len(content) != 32 or any(isinstance(item, bool) or not isinstance(item, int) or item < 0 or item > 255 for item in content):
                        raise ContractError("cloud work movement artifact hash bytes are invalid")
                    content = bytes(content).hex()
                if not isinstance(content, str) or not re.fullmatch(r"[0-9a-f]{64}", content):
                    raise ContractError("cloud work movement artifact hash is invalid")
                result.append((seq, content, require_string(runtime.get("run"), "cloud work runtime run", 512), require_string(runtime.get("checkpoint"), "cloud work runtime checkpoint", 512)))
                if len(result) >= self.config["driver_ipc"]["ledger_batch_size"]:
                    break
        return result

    def read_outbound(self, next_seq):
        try:
            return self._index_rows(next_seq)
        except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
            if str(error) != self.index_warning:
                sys.stderr.write(f"cloud work ledger index unavailable; reading ledger truth: {error}\n")
                sys.stderr.flush()
                self.index_warning = str(error)
            return self._ledger_rows(next_seq)

    def materialize(self, content_hash):
        manifest_bytes = read_bounded_regular(
            os.path.join(self.blobs_dir, content_hash),
            self.config["driver_ipc"]["max_cached_manifest_bytes"],
            "cloud work cached manifest",
        )
        if octopus_content_hash(manifest_bytes) != content_hash:
            raise ContractError("cloud work cached manifest failed content-address verification")
        manifest = require_object(json.loads(manifest_bytes), "cloud work cached manifest")
        label = require_string(manifest.get("label"), "cloud work cached artifact label", 256)
        payload_hash = manifest.get("payload_content_hash")
        if not isinstance(payload_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", payload_hash):
            raise ContractError("cloud work cached manifest has no payload content hash")
        payload = read_bounded_regular(
            os.path.join(self.blobs_dir, payload_hash),
            self.config["driver_ipc"]["max_outbound_payload_bytes"],
            "cloud work cached payload",
        )
        if octopus_content_hash(payload) != payload_hash:
            raise ContractError("cloud work cached payload failed content-address verification")
        return label, payload

    def effect_path(self, source_seq):
        return os.path.join(self.effect_dir, f"{source_seq}.json")

    def load_effect(self, source_seq):
        try:
            value = read_json(self.effect_path(source_seq), f"cloud work effect {source_seq}")
        except FileNotFoundError:
            return None
        required = {"schema_version", "source_seq", "source_hash", "request_id", "operation", "status"}
        optional = {"label", "payload", "failure_class", "error", "evidence_seq"}
        if set(value) - required - optional or required - set(value) or value["schema_version"] != 1 or value["source_seq"] != source_seq:
            raise ContractError(f"cloud work effect {source_seq} is invalid")
        if value["status"] not in {"started", "succeeded", "failed", "indeterminate", "evidenced"}:
            raise ContractError(f"cloud work effect {source_seq} has invalid status")
        return value

    def commit_effect(self, source_seq, value):
        atomic_json(self.effect_path(source_seq), value)


class CloudHttpError(RuntimeError):
    def __init__(self, message, status=None, retryable=False):
        super().__init__(message)
        self.status = status
        self.retryable = retryable


class CloudTransport:
    def __init__(self, config):
        self.config = config
        transport = config["transport"]
        self.base_url = require_string(os.environ.get(transport["base_url_env"]), "cloud work base URL", 4096)
        parsed = urllib.parse.urlsplit(self.base_url)
        localhost = parsed.hostname in {"localhost", "127.0.0.1", "::1"}
        if parsed.scheme != "https" and not (parsed.scheme == "http" and localhost):
            raise ContractError("cloud work base URL requires HTTPS, except HTTP loopback")
        if parsed.username or parsed.password or parsed.fragment or not parsed.hostname:
            raise ContractError("cloud work base URL contains unsupported authority data")
        self.base_url = self.base_url.rstrip("/")
        self.token = require_string(os.environ.get(transport["auth_token_env"]), "cloud work auth token", 32768)
        account = os.environ.get(transport["account_id_env"])
        self.account_id = require_string(account, "cloud work account id", 1024) if account else None
        self.last_evidence = None

    def _root(self):
        path = urllib.parse.urlsplit(self.base_url).path.rstrip("/")
        if "/backend-api" in path:
            return self.base_url + "/wham"
        if path.endswith("/api/codex"):
            return self.base_url
        return self.base_url + "/api/codex"

    def _headers(self, request_id=None, json_body=False):
        headers = {
            "Authorization": f"Bearer {self.token}",
            "Accept": "application/json",
            "User-Agent": self.config["transport"]["user_agent"],
        }
        if self.account_id:
            headers["ChatGPT-Account-Id"] = self.account_id
        if request_id:
            headers["Idempotency-Key"] = request_id
        if json_body:
            headers["Content-Type"] = "application/json"
        return headers

    def sanitize(self, value):
        value = str(value)
        for secret in (self.token, self.account_id):
            if secret:
                value = value.replace(secret, "[REDACTED]")
        return value[:4096]

    def request(self, method, path, query=None, body=None, request_id=None):
        url = self._root() + path
        if query:
            url += "?" + urllib.parse.urlencode([(key, value) for key, value in query.items() if value is not None])
        raw = None if body is None else json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        request = urllib.request.Request(url, data=raw, method=method, headers=self._headers(request_id, raw is not None))
        timeout = self.config["transport"]["request_timeout_ms"] / 1000
        try:
            response = urllib.request.urlopen(request, timeout=timeout)
            with response:
                data = response.read(self.config["transport"]["max_response_bytes"] + 1)
                status = response.status
                request_id_header = response.headers.get("x-request-id")
        except urllib.error.HTTPError as error:
            data = error.read(min(self.config["transport"]["max_response_bytes"], 4096))
            text = data.decode("utf-8", errors="replace")
            raise CloudHttpError(
                self.sanitize(f"cloud service {method} {path} returned HTTP {error.code}: {text}"),
                error.code,
                error.code == 429 or error.code >= 500,
            ) from error
        except (urllib.error.URLError, TimeoutError, socket.timeout, OSError) as error:
            raise CloudHttpError(self.sanitize(f"cloud service {method} {path} failed: {error}"), None, True) from error
        if len(data) > self.config["transport"]["max_response_bytes"]:
            raise CloudHttpError(f"cloud service {method} {path} response exceeded its byte bound")
        try:
            value = json.loads(data)
            if not isinstance(value, (dict, list)):
                raise ContractError(f"cloud service {method} {path} response must be an object or array")
        except Exception as error:
            raise CloudHttpError(self.sanitize(f"cloud service {method} {path} returned invalid JSON: {error}")) from error
        self.last_evidence = {"http_status": status, "request_id": request_id_header, "path": path}
        return value

    def list_environments(self, repository_remote=None):
        rows = {}
        repo = parse_repository_remote(repository_remote) if repository_remote else None
        repo_error = None
        if repo:
            path = f"/environments/by-repo/{repo['provider']}/{urllib.parse.quote(repo['owner'], safe='')}/{urllib.parse.quote(repo['repository'], safe='')}"
            try:
                response = self.request("GET", path)
                items = response if isinstance(response, list) else response.get("items", response.get("environments"))
                if isinstance(items, dict):
                    items = items.get("items")
                if not isinstance(items, list):
                    raise ContractError("repository environment response has no array")
                for item in items:
                    normalized = self._environment(item, repo["sanitized"])
                    rows[normalized["id"]] = normalized
            except Exception as error:
                repo_error = self.sanitize(error)
        global_error = None
        try:
            response = self.request("GET", "/environments")
            items = response if isinstance(response, list) else response.get("items", response.get("environments"))
            if not isinstance(items, list):
                raise ContractError("global environment response has no array")
            for item in items:
                normalized = self._environment(item, None)
                previous = rows.get(normalized["id"])
                if previous:
                    if previous["label"] is not None:
                        normalized["label"] = previous["label"]
                    normalized["repository_hint"] = previous["repository_hint"]
                    normalized["pinned"] = normalized["pinned"] or previous["pinned"]
                rows[normalized["id"]] = normalized
        except Exception as error:
            global_error = self.sanitize(error)
            if not rows:
                raise
        values = list(rows.values())
        values.sort(key=lambda item: (not item["pinned"], (item["label"] or "").lower(), item["id"]))
        return {
            "schema_version": 1,
            "environments": values,
            "repository_evidence": repo,
            "partial_failures": {key: value for key, value in (("repository", repo_error), ("global", global_error)) if value},
        }

    def _environment(self, value, repository_hint):
        value = require_object(value, "cloud environment")
        identifier = require_string(value.get("id"), "cloud environment id", 512)
        label = value.get("label")
        if label is not None:
            label = require_string(label, "cloud environment label", 4096)
        count = value.get("task_count")
        if count is not None:
            count = require_int(count, "cloud environment task_count", 0)
        pinned = value.get("is_pinned", False)
        if not isinstance(pinned, bool):
            raise ContractError("cloud environment is_pinned must be a boolean")
        return {"id": identifier, "label": label, "pinned": pinned, "task_count": count, "repository_hint": repository_hint}

    def task_list(self, arguments):
        return self.request(
            "GET",
            "/tasks/list",
            {
                "limit": arguments.get("limit", self.config["contract"]["max_list_limit"]),
                "task_filter": "current",
                "environment_id": arguments.get("environment_id"),
                "cursor": arguments.get("cursor"),
            },
        )

    def details(self, task_id):
        return self.request("GET", f"/tasks/{urllib.parse.quote(task_id, safe='')}")

    def siblings(self, task_id, turn_id):
        return self.request("GET", f"/tasks/{urllib.parse.quote(task_id, safe='')}/turns/{urllib.parse.quote(turn_id, safe='')}/sibling_turns")

    def create(self, arguments, request_id):
        return require_object(
            self.request("POST", "/tasks", body=build_create_body(arguments), request_id=request_id),
            "cloud work create response",
        )


def operation_label(operation):
    return OPERATION_OUTPUT_LABELS[operation]


def details_and_attempts(transport, task_id, turn_id=None, placement=None, include_siblings=False):
    details = transport.details(task_id)
    current = details.get("current_assistant_turn")
    current_id = current.get("id") if isinstance(current, dict) else None
    sibling_response = None
    query_turn = turn_id or current_id
    if query_turn and (include_siblings or turn_id is not None or placement is not None):
        sibling_response = transport.siblings(task_id, query_turn)
    attempts = normalize_attempts(details, sibling_response)
    return details, attempts


def execute_operation(transport, request):
    operation = request["operation"]
    arguments = request["arguments"]
    request_id = request["request_id"]
    if operation == "cloud_work.capabilities.status":
        return {
            "schema_version": 1,
            "request_id": request_id,
            "interface": "world_cloud_work",
            "interface_process": "live",
            "service_transport_configured": True,
            "service_reachable": transport.last_evidence is not None,
            "last_service_evidence": transport.last_evidence,
            "task_statuses": ["pending", "ready", "applied", "error"],
            "attempt_statuses": ["pending", "in_progress", "completed", "failed", "cancelled", "unknown"],
            "max_attempts": transport.config["contract"]["max_attempts"],
            "apply_authority": False,
        }
    if operation == "cloud_work.environment.list":
        result = transport.list_environments(arguments.get("repository_remote"))
        return {"request_id": request_id, **result, "service_evidence": transport.last_evidence}
    if operation == "cloud_work.task.create":
        response = transport.create(arguments, request_id)
        task = response.get("task") if isinstance(response.get("task"), dict) else response
        task_id = require_string(task.get("id"), "cloud work create response task id", 512)
        return {
            "schema_version": 1,
            "request_id": request_id,
            "task_id": task_id,
            "status": "pending",
            "environment_id": arguments["environment_id"],
            "git_ref": arguments["git_ref"],
            "attempts_requested": arguments.get("attempts", 1),
            "starting_diff_ref": arguments.get("starting_diff", {}).get("artifact_ref"),
            "starting_diff_sha256": arguments.get("starting_diff", {}).get("sha256"),
            "service_evidence": transport.last_evidence,
        }
    if operation == "cloud_work.task.list":
        result = normalize_task_list(transport.task_list(arguments), arguments.get("include_review", False))
        return {"request_id": request_id, **result, "service_evidence": transport.last_evidence}
    task_id = normalize_task_id(arguments["task_id"], transport.config["contract"]["max_task_id_chars"])
    if operation == "cloud_work.task.status":
        summary = normalize_task_summary(transport.details(task_id))
        return {"schema_version": 1, "request_id": request_id, **summary, "service_evidence": transport.last_evidence}
    turn_id = arguments.get("turn_id")
    placement = arguments.get("attempt_placement")
    details, attempts = details_and_attempts(transport, task_id, turn_id, placement, operation == "cloud_work.task.attempts")
    if operation == "cloud_work.task.attempts":
        return {
            "schema_version": 1,
            "request_id": request_id,
            "task_id": task_id,
            "attempts": attempts,
            "service_evidence": transport.last_evidence,
        }
    selected = select_attempt(details, attempts, turn_id, placement)
    if operation == "cloud_work.task.result":
        return {
            "schema_version": 1,
            "request_id": request_id,
            "task_id": task_id,
            "prompt": extract_prompt(details),
            "attempt": selected,
            "service_evidence": transport.last_evidence,
        }
    if operation == "cloud_work.task.diff":
        diff = selected.get("diff")
        if not isinstance(diff, str) or not diff:
            raise ContractError("cloud work selected attempt has no diff")
        raw = diff.encode("utf-8")
        if len(raw) > transport.config["contract"]["max_diff_bytes"]:
            raise ContractError("cloud work selected diff exceeds its byte bound")
        return {
            "schema_version": 1,
            "request_id": request_id,
            "task_id": task_id,
            "attempt": {key: selected.get(key) for key in ("turn_id", "placement", "status")},
            "ref": f"cloud-work://tasks/{urllib.parse.quote(task_id, safe='')}/turns/{urllib.parse.quote(str(selected.get('turn_id') or 'current'), safe='')}/diff",
            "sha256": hashlib.sha256(raw).hexdigest(),
            "size_bytes": len(raw),
            "unified_diff": diff,
            "applied": False,
            "service_evidence": transport.last_evidence,
        }
    if operation == "cloud_work.task.apply_candidate":
        candidate = build_apply_candidate(task_id, normalize_task_summary(details), selected, arguments["target"])
        if candidate["source"]["diff"]["size_bytes"] > transport.config["contract"]["max_diff_bytes"]:
            raise ContractError("cloud work apply candidate exceeds its diff byte bound")
        return {"request_id": request_id, **candidate, "service_evidence": transport.last_evidence}
    raise ContractError(f"cloud work operation {operation!r} is unsupported")


def progress(runtime, source_seq, request, phase):
    return runtime.hand_json(
        "cloud_work.progress",
        {
            "schema_version": 1,
            "source_movement_seq": source_seq,
            "request_id": request["request_id"],
            "operation": request["operation"],
            "phase": phase,
        },
        f"cloud-work-progress-{source_seq}-{phase}",
    )


def sanitize_error(transport, error):
    if transport is None:
        return str(error)[:4096]
    return transport.sanitize(error)


def process_outbound(runtime, transport, source_seq, content_hash, run, checkpoint):
    try:
        label, payload_bytes = runtime.materialize(content_hash)
        if label not in runtime.config["contract"]["accepted_labels"]:
            raise ContractError(f"cloud work interface rejects artifact label {label!r}")
        request = validate_request(require_object(json.loads(payload_bytes), "cloud work request payload"), runtime.config)
    except Exception as error:
        runtime.hand_json(
            "cloud_work.failure",
            {
                "schema_version": 1,
                "source_movement_seq": source_seq,
                "failure_class": "boundary_rejected",
                "error": sanitize_error(transport, error),
            },
            f"cloud-work-failure-{source_seq}",
        )
        return
    effect = runtime.load_effect(source_seq)
    if effect is not None and effect["source_hash"] != content_hash:
        raise ContractError("cloud work effect journal source hash mismatched")
    if effect is not None and effect["status"] == "evidenced":
        return
    if effect is not None and effect["status"] == "succeeded":
        evidence_seq = runtime.hand_json(effect["label"], effect["payload"], f"cloud-work-result-{source_seq}")
        effect.update({"status": "evidenced", "evidence_seq": evidence_seq})
        runtime.commit_effect(source_seq, effect)
        return
    if effect is not None and effect["status"] in {"failed", "indeterminate"}:
        evidence_seq = runtime.hand_json(
            "cloud_work.failure",
            {
                "schema_version": 1,
                "source_movement_seq": source_seq,
                "request_id": effect["request_id"],
                "operation": effect["operation"],
                "failure_class": effect.get("failure_class", "indeterminate_external_effect"),
                "error": effect.get("error"),
            },
            f"cloud-work-result-{source_seq}",
        )
        effect.update({"status": "evidenced", "evidence_seq": evidence_seq})
        runtime.commit_effect(source_seq, effect)
        return
    if effect is not None and effect["status"] == "started" and request["operation"] not in SAFE_OPERATIONS:
        effect.update({"status": "indeterminate", "failure_class": "indeterminate_external_effect", "error": "cloud work create may have reached the service before process interruption; it was not retried"})
        runtime.commit_effect(source_seq, effect)
        process_outbound(runtime, transport, source_seq, content_hash, run, checkpoint)
        return
    effect = {
        "schema_version": 1,
        "source_seq": source_seq,
        "source_hash": content_hash,
        "request_id": request["request_id"],
        "operation": request["operation"],
        "status": "started",
    }
    runtime.commit_effect(source_seq, effect)
    progress(runtime, source_seq, request, "accepted")
    progress(runtime, source_seq, request, "service_call_started")
    try:
        payload = execute_operation(transport, request)
    except ContractError as error:
        effect.update({"status": "failed", "failure_class": "boundary_rejected", "error": sanitize_error(transport, error)})
        runtime.commit_effect(source_seq, effect)
        process_outbound(runtime, transport, source_seq, content_hash, run, checkpoint)
        return
    except Exception as error:
        status = "failed" if request["operation"] in SAFE_OPERATIONS else "indeterminate"
        failure_class = "transport_failure" if status == "failed" else "indeterminate_external_effect"
        effect.update({"status": status, "failure_class": failure_class, "error": sanitize_error(transport, error)})
        runtime.commit_effect(source_seq, effect)
        process_outbound(runtime, transport, source_seq, content_hash, run, checkpoint)
        return
    progress(runtime, source_seq, request, "service_call_finished")
    effect.update({"status": "succeeded", "label": operation_label(request["operation"]), "payload": payload})
    runtime.commit_effect(source_seq, effect)
    process_outbound(runtime, transport, source_seq, content_hash, run, checkpoint)


def main():
    interface_dir = os.environ["OCTOPUS_INTERFACE_DIR"]
    if os.environ["OCTOPUS_INTERFACE_ID"] != "world_cloud_work":
        raise ContractError("cloud work life was started for the wrong exact interface")
    config = load_config(os.path.join(interface_dir, "config.json"))
    validate_activation(read_json(os.path.join(interface_dir, "activation.json"), "cloud work activation"))
    load_operation_catalog(interface_dir)
    transport = CloudTransport(config)
    runtime = RuntimeIO(config)
    runtime.start()
    runtime.ready()
    sys.stderr.write("cloud work interface live: exact_interface=world_cloud_work apply_authority=false\n")
    sys.stderr.flush()
    next_seq = runtime.load_cursor()
    while True:
        rows = runtime.read_outbound(next_seq)
        for source_seq, content_hash, run, checkpoint in rows:
            try:
                process_outbound(runtime, transport, source_seq, content_hash, run, checkpoint)
            except Exception as error:
                sys.stderr.write(f"cloud work movement {source_seq} retained after error: {sanitize_error(transport, error)}\n")
                sys.stderr.flush()
                time.sleep(0.25)
                break
            next_seq = source_seq + 1
            runtime.commit_cursor(next_seq)
        time.sleep(config["driver_ipc"]["idle_sleep_ms"] / 1000)


if __name__ == "__main__":
    main()
