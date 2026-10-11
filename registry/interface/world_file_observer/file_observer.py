import base64
import json
import os
import re
import stat
from pathlib import Path


def object_keys(value, keys, name):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise ValueError(f"{name} must contain exactly {sorted(keys)}")
    return value


def integer(value, minimum, maximum, name):
    if type(value) is not int or not minimum <= value <= maximum:
        raise ValueError(f"{name} must be an integer in [{minimum}, {maximum}]")
    return value


def identifier(value, name):
    if not isinstance(value, str) or not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,127}", value):
        raise ValueError(f"{name} must be a registry identifier")
    return value


def validate_config(value, interface):
    object_keys(value, {"schema_version", "interface", "poll_interval_ms", "read_roots", "files"}, "observer config")
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise ValueError("unsupported observer config schema_version")
    if identifier(value["interface"], "interface") != interface:
        raise ValueError("observer config does not name this exact interface")
    integer(value["poll_interval_ms"], 50, 3600000, "poll_interval_ms")
    if not isinstance(value["read_roots"], list) or len(value["read_roots"]) > 64:
        raise ValueError("read_roots must be an array with at most 64 entries")
    roots = set()
    for root in value["read_roots"]:
        if (not isinstance(root, str) or not root.startswith("/") or "\0" in root
                or len(root.encode()) > 4096 or os.path.normpath(root) != root):
            raise ValueError("read root must be a normalized absolute directory path")
        path = Path(root)
        try:
            canonical = path.resolve(strict=True)
        except (OSError, RuntimeError) as error:
            raise ValueError(f"read root must be an existing canonical directory: {root}") from error
        if str(canonical) != root or not path.is_dir():
            raise ValueError(f"read root must be an existing canonical directory: {root}")
        if path in roots:
            raise ValueError("read_roots must be unique")
        roots.add(path)
    if not isinstance(value["files"], list) or len(value["files"]) > 64:
        raise ValueError("files must be an array with at most 64 entries")
    ids, paths, labels = set(), set(), set()
    for source in value["files"]:
        object_keys(source, {"id", "path", "label", "max_bytes"}, "observed file")
        identifier(source["id"], "observed file id")
        path, label = source["path"], source["label"]
        if (not isinstance(path, str) or not path.startswith("/") or "\0" in path
                or len(path.encode()) > 4096 or os.path.normpath(path) != path or path == "/"):
            raise ValueError("observed path must be an exact normalized absolute file path")
        if not any(root in Path(path).parents for root in roots):
            raise ValueError(f"observed file must be beneath an explicitly declared read root: {path}")
        if (not isinstance(label, str) or not label or label.strip() != label
                or len(label.encode()) > 256):
            raise ValueError("observed label must be a nonempty trimmed string of at most 256 bytes")
        integer(source["max_bytes"], 1, 262144, "max_bytes")
        if source["id"] in ids or path in paths or label in labels:
            raise ValueError("observed file ids, paths and labels must each be unique")
        ids.add(source["id"])
        paths.add(path)
        labels.add(label)
    return value


def validate_authority(life, config):
    processes = life.get("processes") if isinstance(life, dict) else None
    if not isinstance(processes, list) or len(processes) != 1:
        raise ValueError("file observer requires one declared life process")
    authority = processes[0]["authority"]
    expected_paths = sorted(config["read_roots"])
    if sorted(authority["read_paths"]) != expected_paths:
        raise ValueError("observer read_roots and life authority.read_paths must match exactly")
    for key in ("read_root_slots", "write_root_slots", "executable_root_slots", "write_paths",
                "executable_paths", "read_env_paths", "write_env_paths", "executable_env_paths"):
        if authority[key] != []:
            raise ValueError(f"observer authority.{key} must be empty")
    if authority["allow_network"] is not False or authority["allow_local_ipc"] is not False:
        raise ValueError("file observer needs neither network nor local IPC authority")


def read_snapshot(source):
    path = Path(source["path"])
    if str(path.resolve(strict=False)) != str(path):
        raise PermissionError(f"observed path traverses a symlink: {path}")
    try:
        before = path.lstat()
    except FileNotFoundError:
        return None
    if not stat.S_ISREG(before.st_mode):
        raise PermissionError(f"observed path is not a regular file: {path}")
    if before.st_size > source["max_bytes"]:
        raise ValueError(f"observed file exceeds max_bytes: {path}")
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode):
            raise PermissionError(f"observed file changed type: {path}")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            payload = handle.read(source["max_bytes"] + 1)
            after = os.fstat(handle.fileno())
        linked = path.lstat()
        def identity(value):
            return value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns
        if (identity(before) != identity(opened) or identity(opened) != identity(after)
                or identity(after) != identity(linked) or len(payload) != after.st_size
                or len(payload) > source["max_bytes"] or str(path.resolve()) != str(path)):
            raise RuntimeError(f"observed file changed during snapshot: {path}")
        return payload
    finally:
        if descriptor >= 0:
            os.close(descriptor)


class FileObserver:
    def __init__(self, config, boundary):
        self.boundary = boundary
        self.config = validate_config(config, boundary.INTERFACE_ID)
        self.state_path = os.path.join(boundary.VAR_DIR, "file-observer-state.json")
        contract = {key: value for key, value in self.config.items() if key != "poll_interval_ms"}
        self.config_hash = boundary.octopus_content_hash(boundary.canonical_json(contract))
        try:
            self.state = json.loads(boundary.stable_read(
                self.state_path, boundary.IO_LIMITS["max_control_frame_bytes"],
                "file observer state", private=True))
        except FileNotFoundError:
            self.state = {"schema_version": 1, "interface": boundary.INTERFACE_ID,
                          "config_hash": self.config_hash, "files": {}}
        self.validate_state()

    def validate_state(self):
        state = object_keys(self.state, {"schema_version", "interface", "config_hash", "files"}, "observer state")
        if (type(state["schema_version"]) is not int or state["schema_version"] != 1
                or state["interface"] != self.boundary.INTERFACE_ID
                or state["config_hash"] != self.config_hash):
            raise ValueError("observer state/config identity differs; author a new interface for a new observation contract")
        if not isinstance(state["files"], dict):
            raise ValueError("observer state.files must be an object")
        sources = {source["id"]: source for source in self.config["files"]}
        if not state["files"].keys() <= sources.keys():
            raise ValueError("observer state contains an undeclared file")
        for key, row in state["files"].items():
            object_keys(row, {"revision", "content_hash", "pending", "admitted_seq"}, "observed state")
            integer(row["revision"], 1, 2**63 - 1, "observation revision")
            if row["content_hash"] is not None:
                self.boundary.content_hexdigest(row["content_hash"])
            if row["admitted_seq"] is not None:
                integer(row["admitted_seq"], 0, 2**63 - 1, "admitted_seq")
            pending = row["pending"]
            if pending is not None:
                object_keys(pending, {"manifest_path", "manifest_hash"}, "pending observation")
                digest = self.boundary.content_hexdigest(pending["manifest_hash"])
                expected = os.path.join(self.boundary.PAYLOADS_DIR, f"{key}-{row['revision']}-{digest}.artifact.json")
                if pending["manifest_path"] != expected or row["content_hash"] is None or row["admitted_seq"] is not None:
                    raise ValueError("pending observation identity is inconsistent")
            elif row["content_hash"] is not None and row["admitted_seq"] is None:
                raise ValueError("present observation has neither pending delivery nor admission evidence")

    def save(self):
        self.boundary.atomic_json(self.state_path, self.state)

    def deliver_pending(self, source, row):
        pending = row["pending"]
        if pending is None:
            return
        boundary = self.boundary
        manifest_bytes = boundary.stable_read(pending["manifest_path"], boundary.IO_LIMITS["max_manifest_bytes"], "pending observation manifest", private=True)
        if boundary.octopus_content_hash(manifest_bytes) != pending["manifest_hash"]:
            raise ValueError("pending observation manifest integrity mismatch")
        manifest = object_keys(json.loads(manifest_bytes), {"label", "payload_path", "payload_content_hash"}, "observation manifest")
        payload_hash = boundary.content_hexdigest(manifest["payload_content_hash"])
        expected = os.path.join(boundary.PAYLOADS_DIR, f"{source['id']}-{row['revision']}-{payload_hash}.payload")
        if manifest["label"] != source["label"] or manifest["payload_path"] != expected:
            raise ValueError("pending observation manifest does not match the configured source")
        payload = boundary.stable_read(expected, boundary.IO_LIMITS["max_payload_bytes"], "pending observation payload", private=True)
        if boundary.octopus_content_hash(payload) != payload_hash:
            raise ValueError("pending observation payload integrity mismatch")
        row["admitted_seq"] = boundary.hand_artifact_idempotent(pending["manifest_path"], source["label"], pending["manifest_hash"])
        row["pending"] = None
        self.save()

    def poll_once(self):
        boundary = self.boundary
        for source in self.config["files"]:
            row = self.state["files"].get(source["id"])
            if row is not None:
                self.deliver_pending(source, row)
            content = read_snapshot(source)
            digest = boundary.octopus_content_hash(content) if content is not None else None
            if row is not None and row["content_hash"] == digest:
                continue
            revision = 1 if row is None else row["revision"] + 1
            row = {"revision": revision, "content_hash": digest, "pending": None, "admitted_seq": None}
            if content is not None:
                payload = boundary.canonical_json({
                    "schema_version": 1, "interface": boundary.INTERFACE_ID,
                    "source_id": source["id"], "source_path": source["path"], "revision": revision,
                    "content_hash": digest, "size_bytes": len(content),
                    "content_base64": base64.b64encode(content).decode("ascii"),
                })
                manifest_path, manifest_hash = boundary.materialize_artifact(source["label"], payload, f"{source['id']}-{revision}")
                row["pending"] = {"manifest_path": manifest_path, "manifest_hash": manifest_hash}
            self.state["files"][source["id"]] = row
            self.save()
            self.deliver_pending(source, row)

    def inspection(self):
        return json.loads(json.dumps({
            "interface": self.boundary.INTERFACE_ID,
            "config_hash": self.config_hash,
            "observations": [{"source": source, "delivery": self.state["files"].get(source["id"])}
                             for source in self.config["files"]],
        }))
