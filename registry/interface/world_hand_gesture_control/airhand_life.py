import json
import math
import os
import select
import socket
import stat
import sys
import time
import uuid

from octopus_content_hash import octopus_content_hash


INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.environ["OCTOPUS_INTERFACE_DIR"]
VAR_DIR = os.environ["OCTOPUS_VAR_DIR"]
MOVEMENTS_JSONL = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
BLOBS_DIR = os.environ["OCTOPUS_BLOBS_DIR"]


def fail(message):
    raise RuntimeError(message)


def exact_object(value, required, optional=()):
    if not isinstance(value, dict):
        fail("value must be a JSON object")
    keys = set(value)
    required = set(required)
    optional = set(optional)
    missing = sorted(required - keys)
    unknown = sorted(keys - required - optional)
    if missing:
        fail(f"missing fields: {missing}")
    if unknown:
        fail(f"unknown fields: {unknown}")
    return value


def bounded_string(value, name, maximum=4096):
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > maximum:
        fail(f"{name} must be a non-empty string of at most {maximum} bytes")
    return value


def strict_bool(value, name):
    if type(value) is not bool:
        fail(f"{name} must be a boolean")
    return value


def strict_integer(value, name, minimum=0):
    if type(value) is not int or value < minimum:
        fail(f"{name} must be an integer >= {minimum}")
    return value


def interface_endpoint(value):
    if not isinstance(value, dict) or set(value) != {"Interface"}:
        return None
    identifier = value.get("Interface")
    if not isinstance(identifier, str) or not identifier or identifier.strip() != identifier:
        return None
    return identifier


def outbound_runtime(movement):
    metadata = movement.get("invocation_metadata")
    if not isinstance(metadata, dict):
        fail("outbound movement invocation_metadata must be an object")
    runtime = metadata.get("runtime")
    if not isinstance(runtime, dict):
        fail("outbound movement runtime metadata must be an object")
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        value = bounded_string(runtime.get(field), f"outbound runtime {field}", 256)
        if value.strip() != value:
            fail(f"outbound runtime {field} must be trimmed")
    strict_integer(runtime.get("epoch"), "outbound runtime epoch")
    if runtime["interface"] != INTERFACE_ID:
        fail("outbound runtime interface does not match its exact target")
    return runtime


def finite_number(value, name, minimum=None, maximum=None):
    if type(value) not in (int, float) or not math.isfinite(value):
        fail(f"{name} must be a finite number")
    number = float(value)
    if minimum is not None and number < minimum:
        fail(f"{name} must be >= {minimum}")
    if maximum is not None and number > maximum:
        fail(f"{name} must be <= {maximum}")
    return number


def load_contract():
    descriptor = json.loads(os.environ["OCTOPUS_INTERFACE_DESCRIPTOR_JSON"])
    exact_object(
        descriptor,
        {"id", "interface_type", "role", "direction", "source_names", "interface_driver_ref", "interface_driver_config"},
        {"interface_policy"},
    )
    if descriptor["id"] != INTERFACE_ID:
        fail(f"descriptor id {descriptor['id']!r} does not match {INTERFACE_ID!r}")
    if descriptor["interface_type"] != "world" or descriptor["direction"] != "bidirectional":
        fail("hand gesture control must be a bidirectional world interface")
    driver = exact_object(
        descriptor["interface_driver_config"],
        {"private_config"},
        {"residual_ambient", "host_slots", "transport", "supported_reasoning_efforts"},
    )
    metadata = exact_object(
        driver["private_config"],
        {
            "protocol_version",
            "socket_file",
            "command_label",
            "command_result_label",
            "failure_label",
            "event_labels",
            "max_wire_bytes",
            "max_pending_events",
            "event_result_history",
        },
    )
    if strict_integer(metadata["protocol_version"], "protocol_version", 1) != 1:
        fail("unsupported hand gesture control protocol_version")
    socket_file = bounded_string(metadata["socket_file"], "socket_file", 128)
    if socket_file in (".", "..") or "/" in socket_file or "\\" in socket_file:
        fail("socket_file must be a plain file name")
    event_labels = metadata["event_labels"]
    if not isinstance(event_labels, list) or not event_labels:
        fail("event_labels must be a non-empty array")
    normalized_labels = [bounded_string(value, "event label", 128) for value in event_labels]
    if len(set(normalized_labels)) != len(normalized_labels):
        fail("event_labels contains duplicates")
    return {
        "protocol_version": 1,
        "socket_file": socket_file,
        "command_label": bounded_string(metadata["command_label"], "command_label", 128),
        "command_result_label": bounded_string(metadata["command_result_label"], "command_result_label", 128),
        "failure_label": bounded_string(metadata["failure_label"], "failure_label", 128),
        "event_labels": frozenset(normalized_labels),
        "max_wire_bytes": strict_integer(metadata["max_wire_bytes"], "max_wire_bytes", 1024),
        "max_pending_events": strict_integer(metadata["max_pending_events"], "max_pending_events", 1),
        "event_result_history": strict_integer(metadata["event_result_history"], "event_result_history", 1),
    }


CONTRACT = load_contract()
SOCKET_PATH = os.path.join(VAR_DIR, CONTRACT["socket_file"])
CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")
EVENT_HISTORY_PATH = os.path.join(VAR_DIR, "event-results.json")
STATE_PATH = os.path.join(VAR_DIR, "bridge-state.json")


def canonical_json(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def private_directory(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    state = os.lstat(path)
    if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
        fail(f"private path is not a directory: {path}")
    if hasattr(os, "geteuid") and state.st_uid != os.geteuid():
        fail(f"private path has the wrong owner: {path}")
    if stat.S_IMODE(state.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


def open_stable_regular(path, name, private=False):
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        fail(f"{name} is not a regular file")
    if private:
        if stat.S_IMODE(before.st_mode) != 0o600:
            fail(f"{name} is not private")
        if hasattr(os, "geteuid") and before.st_uid != os.geteuid():
            fail(f"{name} has the wrong owner")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    opened = os.fstat(descriptor)
    linked = os.lstat(path)
    if (
        not stat.S_ISREG(opened.st_mode)
        or stat.S_ISLNK(linked.st_mode)
        or (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino)
        or (linked.st_dev, linked.st_ino) != (opened.st_dev, opened.st_ino)
    ):
        os.close(descriptor)
        fail(f"{name} changed while opening")
    return descriptor, opened


def stable_read(path, maximum, name, private=False):
    descriptor, opened = open_stable_regular(path, name, private=private)
    if opened.st_size > maximum:
        os.close(descriptor)
        fail(f"{name} exceeds {maximum} bytes")
    try:
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            fail(f"{name} grew beyond {maximum} bytes")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def sync_directory(path):
    directory = os.open(path, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def atomic_bytes(path, content):
    private_directory(os.path.dirname(path))
    temporary = f"{path}.tmp-{os.getpid()}-{time.time_ns()}"
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "wb", closefd=False) as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
    finally:
        os.close(descriptor)
    os.replace(temporary, path)
    sync_directory(os.path.dirname(path))


def atomic_json(path, value):
    atomic_bytes(path, canonical_json(value) + b"\n")


def load_cursor():
    try:
        value = json.loads(stable_read(CURSOR_PATH, CONTRACT["max_wire_bytes"], "ledger cursor", private=True))
    except FileNotFoundError:
        return 0, 0
    exact_object(value, {"version", "offset", "next_seq"})
    if value["version"] != 1:
        fail("unsupported ledger cursor version")
    return (
        strict_integer(value["offset"], "cursor.offset"),
        strict_integer(value["next_seq"], "cursor.next_seq"),
    )


def load_event_history():
    try:
        value = json.loads(stable_read(EVENT_HISTORY_PATH, CONTRACT["max_wire_bytes"], "event result history", private=True))
    except FileNotFoundError:
        return {}, []
    exact_object(value, {"version", "results"})
    if value["version"] != 1 or not isinstance(value["results"], list):
        fail("event result history has an unsupported shape")
    results = {}
    order = []
    for item in value["results"]:
        exact_object(item, {"event_id", "result"})
        event_id = bounded_string(item["event_id"], "event_id", 64)
        if event_id in results:
            fail("event result history contains duplicate event_id values")
        if not isinstance(item["result"], dict):
            fail("event result must be an object")
        results[event_id] = item["result"]
        order.append(event_id)
    return results, order


def validate_event_payload(label, payload):
    if not isinstance(payload, dict):
        fail("event payload must be an object")
    prefix = "hand_gesture_control."
    if not label.startswith(prefix):
        fail("event label is outside the hand gesture control vocabulary")
    name = label[len(prefix):]
    if name == "lifecycle":
        exact_object(payload, {"state", "product", "version"})
        if payload["state"] != "connected" or payload["product"] != "AirHand":
            fail("invalid lifecycle event")
        bounded_string(payload["version"], "version", 64)
    elif name == "tracking":
        exact_object(payload, {"state", "source", "reason"})
        if payload["state"] not in {"start_requested", "stopped"}:
            fail("invalid tracking state")
        if payload["source"] not in {"camera", "window", "phone"}:
            fail("invalid tracking source")
        bounded_string(payload["reason"], "reason", 256)
    elif name == "gesture":
        exact_object(payload, {"gesture", "mode", "confidence"})
        if payload["gesture"] not in {"none", "aim", "open_palm", "left_click", "right_click", "fist", "mission_control"}:
            fail("invalid gesture")
        if payload["mode"] not in {"stopped", "searching", "pointing", "scrolling", "unavailable"}:
            fail("invalid tracking mode")
        finite_number(payload["confidence"], "confidence", 0, 1)
    elif name == "pointer_effect":
        exact_object(payload, {"action"}, {"x", "y"})
        if payload["action"] not in {"left_down", "left_up", "right_click", "space_left", "space_right"}:
            fail("invalid pointer effect")
        if "x" in payload:
            finite_number(payload["x"], "x")
        if "y" in payload:
            finite_number(payload["y"], "y")
        if ("x" in payload) != ("y" in payload):
            fail("pointer coordinates must be present together")
    elif name == "calibration":
        exact_object(payload, {"state", "message", "progress"}, {"horizontal_quality", "vertical_quality"})
        if payload["state"] not in {"started", "cancelled", "cleared", "saved", "failed"}:
            fail("invalid calibration state")
        bounded_string(payload["message"], "message", 1024)
        finite_number(payload["progress"], "progress", 0, 1)
        for key in ("horizontal_quality", "vertical_quality"):
            if key in payload:
                finite_number(payload[key], key, 0, 1)
    elif name == "training":
        exact_object(payload, {"state", "message", "take_count"})
        if payload["state"] not in {"target_started", "target_stopped", "hand_started", "hand_stopped", "saved", "cleared", "failed"}:
            fail("invalid training state")
        bounded_string(payload["message"], "message", 1024)
        strict_integer(payload["take_count"], "take_count")
    elif name == "permissions":
        exact_object(payload, {"camera", "accessibility", "summary"})
        if payload["camera"] not in {"authorized", "not_determined", "denied", "restricted", "unknown"}:
            fail("invalid camera permission state")
        strict_bool(payload["accessibility"], "accessibility")
        bounded_string(payload["summary"], "summary", 512)
    else:
        fail("unknown event label")


SETTING_SPECS = {
    "mirror_x": ("bool", None, None),
    "preview_mirror_x": ("bool", None, None),
    "invert_y": ("bool", None, None),
    "invert_scroll": ("bool", None, None),
    "start_tracking_on_launch": ("bool", None, None),
    "magnetic_targeting": ("bool", None, None),
    "auto_gait_compensation": ("bool", None, None),
    "show_overlay": ("bool", None, None),
    "finger_throttle": ("bool", None, None),
    "magnetic_strength": ("number", 0, 1),
    "smoothing": ("number", 0, 0.99),
    "dead_zone_pixels": ("number", 0, 200),
    "pointer_sensitivity": ("number", 0.05, 4),
    "stabilization_radius_pixels": ("number", 0, 400),
    "confidence_threshold": ("number", 0, 1),
    "click_hold_seconds": ("number", 0.01, 2),
    "click_cooldown_seconds": ("number", 0, 5),
    "scroll_sensitivity": ("number", 1, 10000),
    "camera_margin_x": ("number", 0, 0.45),
    "camera_margin_y": ("number", 0, 0.45),
    "tuning_preset": ("enum", {"treadmill", "balanced", "precision", "fast", "custom"}, None),
    "control_mode": ("enum", {"smartWalk", "direct"}, None),
}


def validate_command(payload):
    exact_object(payload, {"schema_version", "command", "arguments"})
    if payload["schema_version"] != 1:
        fail("unsupported command schema_version")
    command = bounded_string(payload["command"], "command", 128)
    arguments = payload["arguments"]
    if not isinstance(arguments, dict):
        fail("command arguments must be an object")
    if command == "tracking.set":
        exact_object(arguments, {"enabled"})
        strict_bool(arguments["enabled"], "enabled")
    elif command == "source.set":
        exact_object(arguments, {"source"})
        if arguments["source"] not in {"camera", "window", "phone"}:
            fail("source must be camera, window, or phone")
    elif command in {"source.refresh", "status.read", "calibration.start", "calibration.cancel", "calibration.clear", "training.clear"}:
        exact_object(arguments, set())
    elif command == "settings.update":
        exact_object(arguments, {"values"})
        values = arguments["values"]
        if not isinstance(values, dict) or not values:
            fail("settings.update values must be a non-empty object")
        unknown = sorted(set(values) - set(SETTING_SPECS))
        if unknown:
            fail(f"unknown setting fields: {unknown}")
        for key, value in values.items():
            shape, lower, upper = SETTING_SPECS[key]
            if shape == "bool":
                strict_bool(value, key)
            elif shape == "number":
                finite_number(value, key, lower, upper)
            elif not isinstance(value, str) or value not in lower:
                fail(f"invalid {key}")
    else:
        fail(f"unknown command {command!r}")
    return {"schema_version": 1, "command": command, "arguments": arguments}


def cached_artifact(content_hash):
    if not isinstance(content_hash, str) or len(content_hash) != 64 or any(char not in "0123456789abcdef" for char in content_hash):
        fail("outbound artifact has no valid content hash")
    manifest_path = os.path.join(BLOBS_DIR, content_hash)
    manifest_bytes = stable_read(manifest_path, CONTRACT["max_wire_bytes"], "cached artifact manifest")
    if octopus_content_hash(manifest_bytes) != content_hash:
        fail("cached artifact manifest hash mismatch")
    manifest = json.loads(manifest_bytes)
    exact_object(manifest, {"label", "payload_path"}, {"payload_content_hash"})
    label = bounded_string(manifest["label"], "artifact label", 256)
    payload_hash = manifest.get("payload_content_hash")
    if not isinstance(payload_hash, str) or len(payload_hash) != 64 or any(char not in "0123456789abcdef" for char in payload_hash):
        fail("cached artifact manifest has no valid payload_content_hash")
    payload_path = os.path.join(BLOBS_DIR, payload_hash)
    payload = stable_read(payload_path, CONTRACT["max_wire_bytes"], "cached artifact payload")
    if octopus_content_hash(payload) != payload_hash:
        fail("cached artifact payload hash mismatch")
    return label, payload


def artifact_file(label, payload, stem):
    payload_bytes = canonical_json(payload)
    if len(payload_bytes) > CONTRACT["max_wire_bytes"]:
        fail("artifact payload exceeds max_wire_bytes")
    digest = octopus_content_hash(payload_bytes)
    safe_stem = "".join(character if character.isalnum() or character in "-_" else "-" for character in stem)[:96]
    payload_path = os.path.join(VAR_DIR, f"{safe_stem}-{digest}.payload.json")
    if os.path.exists(payload_path):
        if pathlib_bytes(payload_path) != payload_bytes:
            fail("existing artifact payload does not match its content identity")
    else:
        atomic_bytes(payload_path, payload_bytes)
    manifest = {"label": label, "payload_path": payload_path, "payload_content_hash": digest}
    manifest_bytes = canonical_json(manifest)
    manifest_digest = octopus_content_hash(manifest_bytes)
    manifest_path = os.path.join(VAR_DIR, f"{safe_stem}-{manifest_digest}.artifact.json")
    if os.path.exists(manifest_path):
        if pathlib_bytes(manifest_path) != manifest_bytes:
            fail("existing artifact manifest does not match its content identity")
    else:
        atomic_bytes(manifest_path, manifest_bytes)
    return manifest_path


def pathlib_bytes(path):
    return stable_read(path, CONTRACT["max_wire_bytes"], "private artifact file", private=True)


class BridgeLife:
    def __init__(self):
        private_directory(VAR_DIR)
        self.offset, self.next_seq = load_cursor()
        self.event_results, self.event_order = load_event_history()
        self.request_id = 1
        self.pending_arrivals = {}
        self.pending_command = None
        self.stdin_buffer = bytearray()
        self.client_buffer = bytearray()
        self.client = None
        self.client_hello = False
        self.kqueue = select.kqueue()
        self.ledger_fd = None
        self.ledger_directory_fd = None
        self.server = self.open_server()
        self.register_read(self.server.fileno())
        self.register_read(sys.stdin.fileno())
        self.watch_ledger()
        self.write_state("listening")

    def open_server(self):
        if len(SOCKET_PATH.encode("utf-8")) >= 104:
            fail("bridge socket path exceeds the macOS AF_UNIX limit; use a shorter OCTOPUS_HOME")
        try:
            metadata = os.lstat(SOCKET_PATH)
        except FileNotFoundError:
            metadata = None
        if metadata is not None:
            if not stat.S_ISSOCK(metadata.st_mode):
                fail("bridge socket path exists and is not a socket")
            os.unlink(SOCKET_PATH)
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.setblocking(False)
        server.bind(SOCKET_PATH)
        os.chmod(SOCKET_PATH, 0o600)
        server.listen(4)
        return server

    def register_read(self, descriptor):
        self.kqueue.control(
            [select.kevent(descriptor, filter=select.KQ_FILTER_READ, flags=select.KQ_EV_ADD | select.KQ_EV_ENABLE | select.KQ_EV_CLEAR)],
            0,
            0,
        )

    def unregister(self, descriptor):
        try:
            self.kqueue.control([select.kevent(descriptor, filter=select.KQ_FILTER_READ, flags=select.KQ_EV_DELETE)], 0, 0)
        except OSError:
            pass

    def watch_ledger(self):
        if self.ledger_fd is not None:
            try:
                self.kqueue.control([select.kevent(self.ledger_fd, filter=select.KQ_FILTER_VNODE, flags=select.KQ_EV_DELETE)], 0, 0)
            except OSError:
                pass
            os.close(self.ledger_fd)
            self.ledger_fd = None
        try:
            self.ledger_fd = os.open(MOVEMENTS_JSONL, os.O_RDONLY)
        except FileNotFoundError:
            if self.ledger_directory_fd is None:
                self.ledger_directory_fd = os.open(os.path.dirname(MOVEMENTS_JSONL), os.O_RDONLY)
                self.kqueue.control(
                    [select.kevent(self.ledger_directory_fd, filter=select.KQ_FILTER_VNODE, flags=select.KQ_EV_ADD | select.KQ_EV_ENABLE | select.KQ_EV_CLEAR, fflags=select.KQ_NOTE_WRITE | select.KQ_NOTE_EXTEND | select.KQ_NOTE_RENAME | select.KQ_NOTE_DELETE)],
                    0,
                    0,
                )
            return
        self.kqueue.control(
            [select.kevent(self.ledger_fd, filter=select.KQ_FILTER_VNODE, flags=select.KQ_EV_ADD | select.KQ_EV_ENABLE | select.KQ_EV_CLEAR, fflags=select.KQ_NOTE_WRITE | select.KQ_NOTE_EXTEND | select.KQ_NOTE_RENAME | select.KQ_NOTE_DELETE)],
            0,
            0,
        )

    def write_state(self, state):
        atomic_json(
            STATE_PATH,
            {
                "version": 1,
                "interface": INTERFACE_ID,
                "state": state,
                "socket_path": SOCKET_PATH,
                "app_connected": self.client is not None and self.client_hello,
                "pending_command_seq": None if self.pending_command is None else self.pending_command["movement_seq"],
                "cursor": {"offset": self.offset, "next_seq": self.next_seq},
                "pid": os.getpid(),
            },
        )

    def commit_cursor(self, offset, next_seq):
        atomic_json(CURSOR_PATH, {"version": 1, "offset": offset, "next_seq": next_seq})
        self.offset = offset
        self.next_seq = next_seq
        self.write_state("connected" if self.client_hello else "listening")

    def remember_event_result(self, event_id, result):
        if event_id in self.event_results:
            self.event_order.remove(event_id)
        self.event_results[event_id] = result
        self.event_order.append(event_id)
        while len(self.event_order) > CONTRACT["event_result_history"]:
            removed = self.event_order.pop(0)
            self.event_results.pop(removed, None)
        atomic_json(
            EVENT_HISTORY_PATH,
            {
                "version": 1,
                "results": [{"event_id": item, "result": self.event_results[item]} for item in self.event_order],
            },
        )

    def emit_arrival(self, manifest_path, completion):
        if len(self.pending_arrivals) >= CONTRACT["max_pending_events"]:
            fail("too many pending runtime delivery verdicts")
        request_id = self.request_id
        self.request_id += 1
        self.pending_arrivals[request_id] = completion
        message = {
            "kind": "arrival",
            "request_id": request_id,
            "artifact_manifest_path": manifest_path,
            "evidence": [],
        }
        sys.stdout.buffer.write(canonical_json(message) + b"\n")
        sys.stdout.buffer.flush()

    def send_client(self, value):
        if self.client is None:
            return False
        data = canonical_json(value) + b"\n"
        if len(data) > CONTRACT["max_wire_bytes"]:
            fail("client message exceeds max_wire_bytes")
        try:
            self.client.sendall(data)
            return True
        except OSError:
            self.close_client()
            return False

    def close_client(self):
        if self.client is not None:
            descriptor = self.client.fileno()
            self.unregister(descriptor)
            try:
                self.client.close()
            finally:
                self.client = None
                self.client_hello = False
                self.client_buffer.clear()
        self.write_state("listening")

    def accept_client(self):
        while True:
            try:
                client, _ = self.server.accept()
            except BlockingIOError:
                return
            client.setblocking(True)
            self.close_client()
            self.client = client
            self.client_hello = False
            self.client_buffer.clear()
            self.register_read(client.fileno())
            self.write_state("connected_pending_hello")

    def handle_hello(self, message):
        exact_object(message, {"type", "protocol", "product", "version", "capabilities"})
        if message["protocol"] != CONTRACT["protocol_version"] or message["product"] != "AirHand":
            fail("unsupported AirHand bridge hello")
        bounded_string(message["version"], "version", 64)
        capabilities = message["capabilities"]
        if not isinstance(capabilities, list) or any(not isinstance(item, str) for item in capabilities):
            fail("capabilities must be an array of strings")
        expected = {"commands", "events", "command_results"}
        if set(capabilities) != expected:
            fail("AirHand capabilities do not match the interface contract")
        self.client_hello = True
        self.send_client({"type": "hello_ack", "protocol": 1, "interface": INTERFACE_ID})
        self.write_state("connected")
        self.scan_ledger()

    def handle_event(self, message):
        exact_object(message, {"type", "protocol", "event_id", "label", "payload"})
        if message["protocol"] != 1:
            fail("unsupported event protocol")
        event_id = bounded_string(message["event_id"], "event_id", 64)
        try:
            parsed_id = uuid.UUID(event_id)
        except ValueError as error:
            raise RuntimeError("event_id must be a UUID") from error
        if str(parsed_id) != event_id.lower():
            fail("event_id must use canonical UUID spelling")
        label = bounded_string(message["label"], "label", 128)
        if label not in CONTRACT["event_labels"]:
            fail("event label is not declared by this interface")
        validate_event_payload(label, message["payload"])
        if event_id in self.event_results:
            self.send_client({"type": "event_result", "protocol": 1, "event_id": event_id, "delivery": self.event_results[event_id]})
            return
        manifest = artifact_file(label, message["payload"], f"event-{event_id}")
        self.emit_arrival(manifest, {"kind": "event", "event_id": event_id})

    def handle_command_result(self, message):
        exact_object(message, {"type", "protocol", "source_movement_seq", "status", "result"}, {"error"})
        if message["protocol"] != 1:
            fail("unsupported command result protocol")
        source_seq = strict_integer(message["source_movement_seq"], "source_movement_seq")
        if self.pending_command is None or self.pending_command["movement_seq"] != source_seq:
            fail("command result does not match the pending outbound movement")
        if message["status"] not in {"accepted", "failed"}:
            fail("command result status must be accepted or failed")
        if not isinstance(message["result"], dict):
            fail("command result must be an object")
        if message["status"] == "failed":
            bounded_string(message.get("error"), "error", 2048)
        elif "error" in message:
            fail("accepted command result cannot carry error")
        payload = {
            "source_movement_seq": source_seq,
            "command": self.pending_command["command"]["command"],
            "status": message["status"],
            "result": message["result"],
        }
        if "error" in message:
            payload["error"] = message["error"]
        manifest = artifact_file(CONTRACT["command_result_label"], payload, f"command-result-{source_seq}")
        self.emit_arrival(
            manifest,
            {
                "kind": "command_result",
                "source_movement_seq": source_seq,
                "cursor_offset": self.pending_command["cursor_offset"],
                "next_seq": self.pending_command["next_seq"],
            },
        )

    def handle_client_message(self, message):
        if not isinstance(message, dict) or not isinstance(message.get("type"), str):
            fail("client message must have a string type")
        if not self.client_hello:
            if message["type"] != "hello":
                fail("first client message must be hello")
            self.handle_hello(message)
            return
        if message["type"] == "event":
            self.handle_event(message)
        elif message["type"] == "command_result":
            self.handle_command_result(message)
        else:
            fail("unknown client message type")

    def read_client(self):
        if self.client is None:
            return
        try:
            chunk = self.client.recv(65536)
        except OSError:
            self.close_client()
            return
        if not chunk:
            self.close_client()
            return
        self.client_buffer.extend(chunk)
        if len(self.client_buffer) > CONTRACT["max_wire_bytes"]:
            fail("client receive buffer exceeds max_wire_bytes")
        while b"\n" in self.client_buffer:
            line, _, remainder = self.client_buffer.partition(b"\n")
            self.client_buffer = bytearray(remainder)
            if not line:
                continue
            self.handle_client_message(json.loads(line))

    def read_stdin(self):
        chunk = os.read(sys.stdin.fileno(), 65536)
        if not chunk:
            fail("runtime verdict input closed")
        self.stdin_buffer.extend(chunk)
        if len(self.stdin_buffer) > CONTRACT["max_wire_bytes"]:
            fail("runtime verdict buffer exceeds max_wire_bytes")
        while b"\n" in self.stdin_buffer:
            line, _, remainder = self.stdin_buffer.partition(b"\n")
            self.stdin_buffer = bytearray(remainder)
            if not line:
                continue
            message = json.loads(line)
            exact_object(message, {"kind", "request_id", "verdict"})
            if message["kind"] != "delivery_verdict":
                fail("unexpected runtime input kind")
            request_id = strict_integer(message["request_id"], "request_id", 1)
            completion = self.pending_arrivals.pop(request_id, None)
            if completion is None:
                fail("runtime returned a verdict for an unknown request_id")
            verdict = message["verdict"]
            if not isinstance(verdict, dict) or len(verdict) != 1:
                fail("runtime delivery verdict has an invalid shape")
            if completion["kind"] == "event":
                event_id = completion["event_id"]
                self.remember_event_result(event_id, verdict)
                self.send_client({"type": "event_result", "protocol": 1, "event_id": event_id, "delivery": verdict})
            elif completion["kind"] == "command_result":
                source_seq = completion["source_movement_seq"]
                self.pending_command = None
                self.commit_cursor(completion["cursor_offset"], completion["next_seq"])
                self.send_client({"type": "command_result_ack", "protocol": 1, "source_movement_seq": source_seq, "delivery": verdict})
                self.scan_ledger()
            elif completion["kind"] == "boundary_failure":
                self.pending_command = None
                self.commit_cursor(completion["cursor_offset"], completion["next_seq"])
                self.scan_ledger()
            else:
                fail("unknown pending arrival completion kind")

    def emit_boundary_failure(self, movement_seq, cursor_offset, next_seq, error):
        payload = {
            "source_movement_seq": movement_seq,
            "failure_class": "boundary_rejected",
            "details": {"error": str(error)},
        }
        manifest = artifact_file(CONTRACT["failure_label"], payload, f"boundary-failure-{movement_seq}")
        self.pending_command = {
            "movement_seq": movement_seq,
            "cursor_offset": cursor_offset,
            "next_seq": next_seq,
            "command": {"command": "boundary.failure"},
        }
        self.emit_arrival(
            manifest,
            {
                "kind": "boundary_failure",
                "cursor_offset": cursor_offset,
                "next_seq": next_seq,
            },
        )

    def send_pending_command(self):
        if self.pending_command is None or not self.client_hello:
            return
        self.send_client(
            {
                "type": "command",
                "protocol": 1,
                "source_movement_seq": self.pending_command["movement_seq"],
                "command": self.pending_command["command"]["command"],
                "arguments": self.pending_command["command"]["arguments"],
            }
        )

    def scan_ledger(self):
        if self.pending_command is not None:
            self.send_pending_command()
            return
        try:
            descriptor, ledger_state = open_stable_regular(MOVEMENTS_JSONL, "movement ledger")
        except FileNotFoundError:
            return
        if ledger_state.st_size < self.offset:
            os.close(descriptor)
            fail("movement ledger shrank behind the interface cursor")
        with os.fdopen(descriptor, "rb") as handle:
            handle.seek(self.offset)
            while True:
                start = handle.tell()
                line = handle.readline(CONTRACT["max_wire_bytes"] + 1)
                if not line:
                    return
                if len(line) > CONTRACT["max_wire_bytes"]:
                    fail(f"movement ledger row at byte {start} exceeds max_wire_bytes")
                if not line.endswith(b"\n"):
                    return
                end = handle.tell()
                movement = json.loads(line)
                if not isinstance(movement, dict):
                    fail("movement ledger row must be an object")
                sequence = strict_integer(movement.get("seq"), "movement.seq")
                if sequence != self.next_seq:
                    fail(f"movement ledger sequence discontinuity: expected {self.next_seq}, got {sequence}")
                next_seq = sequence + 1
                if movement.get("kind") != "InterfaceOutbound":
                    self.commit_cursor(end, next_seq)
                    continue
                if interface_endpoint(movement.get("to")) != INTERFACE_ID:
                    self.commit_cursor(end, next_seq)
                    continue
                if interface_endpoint(movement.get("from")) is None:
                    self.commit_cursor(end, next_seq)
                    continue
                outbound_runtime(movement)
                artifact = movement.get("artifact")
                try:
                    if not isinstance(artifact, dict):
                        fail("outbound movement carries no artifact")
                    label, payload_bytes = cached_artifact(artifact.get("content"))
                    if label != CONTRACT["command_label"]:
                        fail(f"unsupported outbound artifact label {label!r}")
                    command = validate_command(json.loads(payload_bytes))
                except (OSError, ValueError, json.JSONDecodeError, RuntimeError) as error:
                    self.emit_boundary_failure(sequence, end, next_seq, error)
                    return
                self.pending_command = {
                    "movement_seq": sequence,
                    "cursor_offset": end,
                    "next_seq": next_seq,
                    "command": command,
                }
                self.write_state("connected" if self.client_hello else "waiting_for_app")
                self.send_pending_command()
                return

    def run(self):
        self.scan_ledger()
        while True:
            events = self.kqueue.control(None, 32, None)
            for event in events:
                if event.ident == self.server.fileno() and event.filter == select.KQ_FILTER_READ:
                    self.accept_client()
                elif event.ident == sys.stdin.fileno() and event.filter == select.KQ_FILTER_READ:
                    self.read_stdin()
                elif self.client is not None and event.ident == self.client.fileno() and event.filter == select.KQ_FILTER_READ:
                    self.read_client()
                elif self.ledger_fd is not None and event.ident == self.ledger_fd and event.filter == select.KQ_FILTER_VNODE:
                    if event.fflags & (select.KQ_NOTE_RENAME | select.KQ_NOTE_DELETE):
                        self.watch_ledger()
                    self.scan_ledger()
                elif self.ledger_directory_fd is not None and event.ident == self.ledger_directory_fd and event.filter == select.KQ_FILTER_VNODE:
                    self.watch_ledger()
                    self.scan_ledger()

    def close(self):
        self.close_client()
        self.write_state("stopped")
        self.unregister(self.server.fileno())
        self.server.close()
        if self.ledger_fd is not None:
            os.close(self.ledger_fd)
        if self.ledger_directory_fd is not None:
            os.close(self.ledger_directory_fd)
        self.kqueue.close()
        try:
            os.unlink(SOCKET_PATH)
        except FileNotFoundError:
            pass


def main():
    life = BridgeLife()
    try:
        sys.stdout.buffer.write(canonical_json({"kind": "ready"}) + b"\n")
        sys.stdout.buffer.flush()
        life.run()
    finally:
        life.close()


if __name__ == "__main__":
    main()
