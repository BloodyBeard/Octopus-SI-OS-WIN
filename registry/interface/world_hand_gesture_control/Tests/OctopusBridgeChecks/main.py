import json
import os
import pathlib
import select
import socket
import subprocess
import sys
import tempfile
import time


ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from octopus_content_hash import octopus_content_hash


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def read_json_line(stream, timeout=5):
    ready, _, _ = select.select([stream], [], [], timeout)
    if not ready:
        raise RuntimeError("timed out waiting for protocol line")
    line = stream.readline()
    if not line:
        raise RuntimeError("protocol stream closed")
    return json.loads(line)


def write_cached_artifact(blobs_dir, label, payload):
    payload_bytes = canonical(payload)
    payload_hash = octopus_content_hash(payload_bytes)
    (blobs_dir / payload_hash).write_bytes(payload_bytes)
    manifest_bytes = canonical(
        {
            "label": label,
            "payload_path": "/source/payload.json",
            "payload_content_hash": payload_hash,
        }
    )
    manifest_hash = octopus_content_hash(manifest_bytes)
    (blobs_dir / manifest_hash).write_bytes(manifest_bytes)
    return manifest_hash


def append_outbound(
    movements,
    sequence,
    content_hash,
    *,
    source=None,
    target=None,
    runtime=None,
):
    if source is None:
        source = {"Interface": "world_provider_test"}
    if target is None:
        target = {"Interface": "world_hand_gesture_control"}
    if runtime is None:
        runtime = {
            "run": "run_hand_gesture_control",
            "ee": "ee_hand_gesture_control",
            "worker": "system_worker_hand_gesture_control",
            "checkpoint": f"cp_hand_gesture_control_{sequence}",
            "interface": "world_hand_gesture_control",
            "epoch": 0,
        }
    movement = {
        "seq": sequence,
        "kind": "InterfaceOutbound",
        "from": source,
        "to": target,
        "invocation_metadata": {"runtime": runtime},
        "artifact": {"id": f"command:{sequence}", "content": content_hash},
    }
    with movements.open("ab", buffering=0) as handle:
        handle.write(canonical(movement) + b"\n")
        os.fsync(handle.fileno())


def hand_verdict(process, request_id, sequence):
    process.stdin.write(
        json.dumps(
            {
                "kind": "delivery_verdict",
                "request_id": request_id,
                "verdict": {"admitted": {"seq": sequence}},
            }
        )
        + "\n"
    )
    process.stdin.flush()


def wait_for_socket(process, socket_path):
    deadline = time.monotonic() + 5
    while not socket_path.exists():
        if process.poll() is not None:
            raise RuntimeError(process.stderr.read())
        if time.monotonic() > deadline:
            raise RuntimeError("bridge socket was not created")
        time.sleep(0.01)


def run_check():
    with tempfile.TemporaryDirectory(dir="/tmp", prefix="airhand-bridge-") as temporary:
        root = pathlib.Path(temporary)
        var_dir = root / "i"
        ledger_dir = root / "l"
        blobs_dir = ledger_dir / "b"
        var_dir.mkdir(parents=True)
        blobs_dir.mkdir(parents=True)
        movements = ledger_dir / "movements.jsonl"
        movements.write_bytes(b"")
        resolved_descriptor = json.loads((ROOT / "interface.json").read_text())
        resolved_descriptor["interface_policy"] = {"variables": {}, "values": {}}
        resolved_descriptor["interface_driver_config"].update(
            {
                "residual_ambient": None,
                "host_slots": {},
                "transport": None,
                "supported_reasoning_efforts": [],
            }
        )
        environment = os.environ.copy()
        environment.update(
            {
                "OCTOPUS_INTERFACE_ID": "world_hand_gesture_control",
                "OCTOPUS_INTERFACE_DIR": str(ROOT),
                "OCTOPUS_VAR_DIR": str(var_dir),
                "OCTOPUS_MOVEMENTS_JSONL": str(movements),
                "OCTOPUS_BLOBS_DIR": str(blobs_dir),
                "OCTOPUS_INTERFACE_DESCRIPTOR_JSON": json.dumps(resolved_descriptor),
            }
        )
        process = subprocess.Popen(
            ["python3", str(ROOT / "airhand_life.py")],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=environment,
        )
        try:
            socket_path = var_dir / "airhand.sock"
            wait_for_socket(process, socket_path)
            client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            client.settimeout(5)
            client.connect(str(socket_path))
            stream = client.makefile("rwb", buffering=0)
            stream.write(
                canonical(
                    {
                        "type": "hello",
                        "protocol": 1,
                        "product": "AirHand",
                        "version": "0.1.0",
                        "capabilities": ["commands", "events", "command_results"],
                    }
                )
                + b"\n"
            )
            hello = json.loads(stream.readline())
            assert hello == {
                "interface": "world_hand_gesture_control",
                "protocol": 1,
                "type": "hello_ack",
            }

            event_id = "11111111-1111-4111-8111-111111111111"
            event = {
                "type": "event",
                "protocol": 1,
                "event_id": event_id,
                "label": "hand_gesture_control.gesture",
                "payload": {"gesture": "aim", "mode": "pointing", "confidence": 0.91},
            }
            stream.write(canonical(event) + b"\n")
            arrival = read_json_line(process.stdout)
            manifest = json.loads(pathlib.Path(arrival["artifact_manifest_path"]).read_text())
            assert manifest["label"] == event["label"]
            assert json.loads(pathlib.Path(manifest["payload_path"]).read_text()) == event["payload"]
            hand_verdict(process, arrival["request_id"], 7)
            event_result = json.loads(stream.readline())
            assert event_result["event_id"] == event_id
            assert event_result["delivery"] == {"admitted": {"seq": 7}}
            stream.write(canonical(event) + b"\n")
            assert json.loads(stream.readline())["delivery"] == {"admitted": {"seq": 7}}
            assert not select.select([process.stdout], [], [], 0.1)[0]

            command_hash = write_cached_artifact(
                blobs_dir,
                "hand_gesture_control.command",
                {"schema_version": 1, "command": "tracking.set", "arguments": {"enabled": True}},
            )
            append_outbound(
                movements,
                0,
                command_hash,
                target={"Interface": "world_other"},
                runtime=7,
            )
            append_outbound(
                movements,
                1,
                command_hash,
                source={"Run": "run_legacy"},
                runtime=7,
            )
            append_outbound(movements, 2, command_hash)
            command = json.loads(stream.readline())
            assert command == {
                "arguments": {"enabled": True},
                "command": "tracking.set",
                "protocol": 1,
                "source_movement_seq": 2,
                "type": "command",
            }
            stream.write(
                canonical(
                    {
                        "type": "command_result",
                        "protocol": 1,
                        "source_movement_seq": 2,
                        "status": "accepted",
                        "result": {"running": True},
                    }
                )
                + b"\n"
            )
            result_arrival = read_json_line(process.stdout)
            result_manifest = json.loads(
                pathlib.Path(result_arrival["artifact_manifest_path"]).read_text()
            )
            assert result_manifest["label"] == "hand_gesture_control.command.result"
            result_payload = json.loads(pathlib.Path(result_manifest["payload_path"]).read_text())
            assert result_payload == {
                "command": "tracking.set",
                "result": {"running": True},
                "source_movement_seq": 2,
                "status": "accepted",
            }
            hand_verdict(process, result_arrival["request_id"], 8)
            result_ack = json.loads(stream.readline())
            assert result_ack["source_movement_seq"] == 2

            malformed_hash = write_cached_artifact(
                blobs_dir,
                "hand_gesture_control.command",
                {"schema_version": 1, "command": "tracking.toggle", "arguments": {}},
            )
            append_outbound(movements, 3, malformed_hash)
            failure_arrival = read_json_line(process.stdout)
            failure_manifest = json.loads(
                pathlib.Path(failure_arrival["artifact_manifest_path"]).read_text()
            )
            assert failure_manifest["label"] == "hand_gesture_control.boundary.failure"
            failure_payload = json.loads(pathlib.Path(failure_manifest["payload_path"]).read_text())
            assert failure_payload["source_movement_seq"] == 3
            assert failure_payload["failure_class"] == "boundary_rejected"
            hand_verdict(process, failure_arrival["request_id"], 9)

            deadline = time.monotonic() + 2
            cursor_path = var_dir / "ledger-cursor.json"
            while True:
                cursor = json.loads(cursor_path.read_text())
                if cursor["next_seq"] == 4:
                    break
                if time.monotonic() > deadline:
                    raise RuntimeError("cursor did not commit the rejected command")
                time.sleep(0.01)
            assert cursor["offset"] == movements.stat().st_size
            state = json.loads((var_dir / "bridge-state.json").read_text())
            assert state["app_connected"] is True
            assert state["pending_command_seq"] is None
            append_outbound(
                movements,
                4,
                command_hash,
                runtime={"interface": "world_hand_gesture_control"},
            )
            deadline = time.monotonic() + 2
            while process.poll() is None and time.monotonic() <= deadline:
                time.sleep(0.01)
            assert process.poll() not in (None, 0)
            assert "outbound runtime run" in process.stderr.read()
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


if __name__ == "__main__":
    run_check()
    print("AirHand Octopus bridge checks passed")
