import copy
import hashlib
import hmac
import http.client
import importlib.util
import io
import json
import os
from pathlib import Path
import queue
import select
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parents[1]
BOUNDARY = HERE.parent / "world_pyecho" / "pyecho_life.py"
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(BOUNDARY.parent))
from github_issues import Receiver, validate_config, make_server
from tunnel import HookManager, HookError, TunnelSupervisor, safe_diagnostic
from relay import RelaySupervisor, RelayError, sse_events

CONFIG = {"schema_version": 1, "enabled": True, "repository": "owner/repo",
          "port": 8787, "path": "/github/private-test-route"}
SECRET = b"x" * 64
PAYLOAD = {"action": "opened", "repository": {"full_name": "owner/repo"},
           "issue": {"id": 123, "number": 4, "title": "An issue", "body": "all details",
                     "labels": [{"name": "bug"}]}, "sender": {"login": "someone"}}
LIMITS = {"max_control_frame_bytes": 1048576, "max_manifest_bytes": 1048576,
          "max_payload_bytes": 8388608, "max_stderr_bytes": 262144}


def environment(root):
    return dict(os.environ, OCTOPUS_INTERFACE_ID="world_github_issues",
                OCTOPUS_VAR_DIR=str(root), OCTOPUS_MOVEMENTS_JSONL=str(root / "movements"),
                OCTOPUS_INTERFACE_HOST_IO_LIMITS=json.dumps(LIMITS),
                PYTHONDONTWRITEBYTECODE="1")


class Contract(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=HERE)
        self.root = Path(self.temp.name)
        with patch.dict(os.environ, environment(self.root)):
            spec = importlib.util.spec_from_file_location("test_boundary", BOUNDARY)
            self.b = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(self.b)
        self.b.private_directory(self.root)
        self.r = Receiver(validate_config(CONFIG), self.b, SECRET)
        self.addCleanup(self.temp.cleanup)
        self.addCleanup(lambda: self.r.close())

    def receive(self, payload=None, event="issues", delivery="a-b-c", signature=None):
        raw = json.dumps(PAYLOAD if payload is None else payload).encode()
        headers = {"X-Hub-Signature-256": signature or "sha256=" + hmac.new(SECRET, raw, hashlib.sha256).hexdigest(),
                   "X-GitHub-Event": event, "X-GitHub-Delivery": delivery}
        return self.r.receive(headers, raw)

    def test_authentication_filter_and_ping(self):
        self.assertEqual(self.receive(signature="sha256=bad")[0], 401)
        wrong = copy.deepcopy(PAYLOAD)
        wrong["repository"]["full_name"] = "foreign/repo"
        self.assertEqual(self.receive(wrong)[0], 403)
        self.assertEqual(self.receive(event="ping")[0], 200)
        self.assertTrue((self.root / "ping.json").exists())
        self.assertEqual(self.receive(event="push")[0], 204)
        closed = dict(PAYLOAD, action="closed")
        self.assertEqual(self.receive(closed)[0], 204)
        pr = copy.deepcopy(PAYLOAD)
        pr["issue"]["pull_request"] = {}
        self.assertEqual(self.receive(pr)[0], 204)
        self.assertEqual(self.r.db.execute("SELECT count(*) FROM deliveries").fetchone()[0], 0)

    def test_refused_restart_retry_duplicate_and_full_payload(self):
        self.assertEqual(self.receive()[0], 202)
        with patch.object(self.b, "hand_artifact_idempotent", side_effect=self.b.DeliveryRefused({"refused": "no_binding"})):
            self.assertTrue(self.r.deliver_one())
        row = self.r.db.execute("SELECT state,seq,manifest,artifact_hash FROM deliveries").fetchone()
        self.assertEqual(row[:2], ("pending", None))
        manifest = json.loads(Path(row[2]).read_text())
        artifact = json.loads(Path(manifest["payload_path"]).read_text())
        self.assertEqual(manifest["label"], "github.issue.opened")
        self.assertEqual(artifact["webhook"], PAYLOAD)
        self.assertEqual(artifact["issue"], PAYLOAD["issue"])
        self.r.close()
        self.r = Receiver(validate_config(CONFIG), self.b, SECRET)
        self.assertEqual(self.receive()[0], 202)
        with self.r.db:
            self.r.db.execute("UPDATE deliveries SET next_attempt=0")
        with patch.object(self.b, "hand_artifact_idempotent", return_value=42) as hand:
            self.assertTrue(self.r.deliver_one())
            self.assertEqual(self.receive(), (200, "delivered"))
            self.assertFalse(self.r.deliver_one())
            hand.assert_called_once_with(row[2], "github.issue.opened", row[3])
        self.assertEqual(json.loads((self.root / "last-admission.json").read_text())["admitted_seq"], 42)
        changed = dict(PAYLOAD, sender={"login": "changed"})
        self.assertEqual(self.receive(changed)[0], 409)

    def test_real_boundary_replays_uncertain_admission_from_ledger(self):
        self.receive()
        row = self.r.db.execute("SELECT manifest,artifact_hash FROM deliveries").fetchone()
        artifact_id = "github.issue.opened:" + row[1]
        (self.root / "movements").write_text(json.dumps({
            "kind": "InterfaceArrival", "from": {"Interface": "world_github_issues"},
            "artifact": {"id": artifact_id}, "seq": 19}) + "\n")
        response = {"kind": "delivery_verdict", "request_id": 1, "verdict": {"refused": {"detail": {
            "artifact_id": artifact_id, "message": "arrival evidence already exists in the movement ledger"}}}}
        stdin, stdout = io.TextIOWrapper(io.BytesIO((json.dumps(response) + "\n").encode())), io.TextIOWrapper(io.BytesIO())
        with patch.object(sys, "stdin", stdin), patch.object(sys, "stdout", stdout):
            self.assertTrue(self.r.deliver_one())
        self.assertEqual(self.r.db.execute("SELECT state,seq FROM deliveries").fetchone(), ("delivered", 19))

    def test_http_limits_path_and_queue_capacity(self):
        self.r.config.update(port=0, max_pending=1)
        server = make_server(self.r)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            def request(path, body, headers):
                c = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=2)
                c.request("POST", path, body=body, headers=headers)
                response = c.getresponse()
                status = response.status
                response.read()
                c.close()
                return status
            headers = {"Content-Type": "application/json", "X-GitHub-Delivery": "id",
                       "X-GitHub-Event": "issues", "X-Hub-Signature-256": "invalid"}
            self.assertEqual(request(CONFIG["path"] + "?x", b"{}", headers), 404)
            self.assertEqual(request(CONFIG["path"], b"{}", headers), 401)
            self.assertEqual(request(CONFIG["path"], b"", dict(headers, **{"Content-Length": "1048577"})), 413)
            self.assertEqual(self.receive()[0], 202)
            self.assertEqual(self.receive(delivery="second")[0], 503)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_hook_uncertainty_recovers_exact_id_and_updates_only_it(self):
        c = dict(CONFIG, tunnel={"gh": "/fake/gh", "cloudflared": "/fake/cloudflared",
                                "metrics_port": 8788, "register_hook": True})
        calls, remote = [], {}
        fail_post = [True]
        def fake_run(args, **kwargs):
            calls.append((args, kwargs))
            method, endpoint = args[args.index("--method") + 1:args.index("--method") + 3]
            body = json.loads(kwargs["input"]) if kwargs["input"] else None
            if method == "GET":
                result = list(remote.values()) if "?" in endpoint else remote[int(endpoint.rsplit("/", 1)[1])]
            elif method == "POST":
                remote[12] = dict(body, id=12)
                if fail_post.pop():
                    raise subprocess.TimeoutExpired(args, 30)
                result = remote[12]
            elif method == "PATCH":
                self.assertTrue(endpoint.endswith("/12"))
                remote[12].update(body)
                result = remote[12]
            return subprocess.CompletedProcess(args, 0, json.dumps(result), "")
        manager = HookManager(c, self.b, SECRET, run=fake_run)
        with self.assertRaisesRegex(HookError, "transport"):
            manager.reconcile("https://first.trycloudflare.com")
        manager = HookManager(c, self.b, SECRET, run=fake_run)
        self.assertEqual(manager.reconcile("https://second.trycloudflare.com"), 12)
        self.assertEqual(sum("POST" in args for args, _ in calls), 1)
        self.assertNotIn(SECRET.decode(), (self.root / "hook.json").read_text())
        for args, kwargs in calls:
            self.assertNotIn(SECRET.decode(), " ".join(args))
        self.assertEqual(remote[12]["events"], ["issues"])
        self.assertEqual(remote[12]["config"]["insecure_ssl"], "0")

    def test_uncertain_creation_without_visibility_never_posts_again(self):
        c = dict(CONFIG, tunnel={"gh": "/fake/gh", "register_hook": True})
        def fake_run(args, **kwargs):
            if "POST" in args:
                raise subprocess.TimeoutExpired(args, 30)
            return subprocess.CompletedProcess(args, 0, "[]", "")
        manager = HookManager(c, self.b, SECRET, run=fake_run)
        with self.assertRaises(HookError):
            manager.reconcile("https://first.trycloudflare.com")
        manager = HookManager(c, self.b, SECRET, run=fake_run)
        with patch.object(manager, "api", wraps=manager.api) as api:
            with self.assertRaisesRegex(HookError, "uncertain"):
                manager.reconcile("https://second.trycloudflare.com")
            self.assertTrue(all(call.args[0] == "GET" for call in api.call_args_list))

    def test_tunnel_child_origin_and_shutdown(self):
        c = dict(CONFIG, tunnel={"gh": "/fake/gh", "cloudflared": "/fake/cloudflared",
                                "metrics_port": 8788, "register_hook": False})
        class Child:
            pid = 123
            stdout = io.BytesIO(b"Assigned https://nice-tunnel.trycloudflare.com\n")
            dead = False
            def poll(self): return 0 if self.dead else None
            def terminate(self): self.dead = True
            def wait(self, timeout): return 0
        child = Child()
        def popen(args, **kwargs):
            self.assertIn("--no-autoupdate", args)
            self.assertIn("http://127.0.0.1:8787", args)
            return child
        supervisor = TunnelSupervisor(c, self.b, SECRET, popen=popen)
        origins = supervisor.start()
        self.assertEqual(origins.get(timeout=2), "https://nice-tunnel.trycloudflare.com")
        supervisor.stop_child()
        self.assertTrue(child.dead)

    def test_tunnel_diagnostic_is_bounded_and_redacted(self):
        raw = b"ERR connection failed token=private-value https://secret.example/a?key=abc /Users/person/.cloudflared/secret " + SECRET + b" " + b"z" * 1000
        diagnostic = safe_diagnostic(raw, SECRET)
        self.assertIn("connection failed", diagnostic)
        for hidden in ("private-value", "secret.example", "/Users/person", SECRET.decode()):
            self.assertNotIn(hidden, diagnostic)
        self.assertLessEqual(len(diagnostic), 512)
        self.assertIsNone(safe_diagnostic(b"INF all is fine", SECRET))

    def relay_config(self):
        return validate_config(dict(CONFIG, relay={"url": "https://smee.io/private-channel",
                                                   "gh": "/fake/gh", "register_hook": False}))

    def envelope(self, event="issues", compact=True):
        payload = copy.deepcopy(PAYLOAD)
        payload["issue"]["body"] = "Unicode: café"
        raw = json.dumps(payload, ensure_ascii=False,
                         separators=(",", ":") if compact else None).encode()
        return json.dumps({"body": payload, "x-github-event": event,
                           "x-github-delivery": event + "-relay",
                           "x-hub-signature-256": "sha256=" + hmac.new(SECRET, raw, hashlib.sha256).hexdigest()}).encode()

    def test_relay_sse_signature_ping_duplicate_and_backpressure(self):
        relay = RelaySupervisor(self.relay_config(), self.b, self.r)
        stop = threading.Event()
        stream = io.BytesIO(b": heartbeat\n\nid: first\ndata: " + self.envelope() + b"\n\n")
        events = list(sse_events(stream, 10000))
        self.assertEqual(len(events), 1)
        identifier, event, data = events[0]
        receive = self.r.receive
        attempts = []
        def accept(headers, body):
            attempts.append(1)
            self.assertIsNone(relay.last_event_id)
            return (503, "queue_full") if len(attempts) == 1 else receive(headers, body)
        with patch.object(self.r, "receive", side_effect=accept) as call:
            with patch.object(stop, "wait", return_value=False):
                relay.process(identifier, data, stop)
            self.assertEqual(call.call_count, 2)
        self.assertEqual(relay.last_event_id, "first")
        relay.process("duplicate", data, stop)
        self.assertEqual(self.r.db.execute("SELECT count(*) FROM deliveries").fetchone()[0], 1)
        relay.process("ping", self.envelope(event="ping"), stop)
        self.assertTrue((self.root / "ping.json").exists())
        self.assertEqual(relay.last_event_id, "ping")
        relay.process("rejected", self.envelope(compact=False), stop)
        self.assertEqual(relay.status["last_delivery"]["http_status"], 401)
        self.assertEqual(relay.last_event_id, "ping")

    def test_relay_bounds_config_and_exact_hook_target(self):
        with self.assertRaises(RelayError):
            list(sse_events(io.BytesIO(b"data: " + b"x" * 101 + b"\n\n"), 100))
        with self.assertRaises(RelayError):
            list(sse_events(io.BytesIO(b"data: " + b"x" * 50 + b"\ndata: " + b"x" * 50 + b"\n\n"), 100))
        c = self.relay_config()
        with self.assertRaises(ValueError):
            validate_config(dict(c, tunnel={}))
        with self.assertRaises(ValueError):
            validate_config(dict(c, relay=dict(c["relay"], url="https://smee.io/channel?token=bad")))
        c["relay"]["register_hook"] = True
        calls = []
        def run(args, **kwargs):
            calls.append(args)
            body = json.loads(kwargs["input"]) if kwargs["input"] else None
            result = [] if "GET" in args else dict(body, id=88)
            return subprocess.CompletedProcess(args, 0, json.dumps(result), "")
        manager = HookManager(c, self.b, SECRET, run=run)
        with self.assertRaises(HookError):
            manager.reconcile_url("https://smee.io/different-channel")
        with self.assertRaises(HookError):
            manager.reconcile("https://first.trycloudflare.com")
        self.assertEqual(calls, [])
        self.assertEqual(manager.reconcile_url(c["relay"]["url"]), 88)
        self.assertTrue(all(args[0] == "/fake/gh" for args in calls))

    def test_relay_connection_lifecycle_no_remote_calls(self):
        stop = threading.Event()
        c = self.relay_config()
        class Response(io.BytesIO):
            status = 200
            def getheader(self, name, default): return "text/event-stream; charset=utf-8"
        response = Response(b"data: " + self.envelope(event="ping") + b"\n\n")
        class Sock:
            def settimeout(self, seconds): pass
            def shutdown(self, how): pass
        class Connection:
            sock = Sock()
            closed = False
            def connect(self): pass
            def request(self, method, path, headers):
                self.requested = method, path, headers
            def getresponse(self): return response
            def close(self):
                self.closed = True
                stop.set()
        connection = Connection()
        relay = RelaySupervisor(c, self.b, self.r, connection_factory=lambda *a, **k: connection)
        relay.run(stop)
        self.assertEqual(connection.requested[:2], ("GET", "/private-channel"))
        self.assertEqual(relay.status["last_delivery"]["status"], "ping")
        self.assertTrue(connection.closed)
        self.assertEqual(relay.status["state"], "stopped")
        self.assertFalse((self.root / "hook.json").exists())

    def test_enabled_life_http_to_real_protocol_refusal_then_admission(self):
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        interface = self.root / "interface"
        interface.mkdir()
        (interface / "interface.json").write_text(json.dumps({
            "interface_driver_config": {"private_config": dict(CONFIG, port=port, retry_seconds=1)}}))
        self.b.atomic_bytes(str(self.root / "webhook.secret"), SECRET)
        env = environment(self.root)
        env["OCTOPUS_INTERFACE_DIR"] = str(interface)
        p = subprocess.Popen([sys.executable, "-B", str(HERE / "github_issues_life.py"), str(BOUNDARY)],
                             env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        def frame():
            self.assertTrue(select.select([p.stdout], [], [], 5)[0])
            return json.loads(p.stdout.readline())
        def post():
            raw = json.dumps(PAYLOAD).encode()
            c = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
            c.request("POST", CONFIG["path"], raw, {
                "Content-Type": "application/json", "X-GitHub-Event": "issues",
                "X-GitHub-Delivery": "process-delivery",
                "X-Hub-Signature-256": "sha256=" + hmac.new(SECRET, raw, hashlib.sha256).hexdigest()})
            result = c.getresponse()
            code = result.status
            result.read()
            c.close()
            return code
        def verdict(request, value):
            p.stdin.write((json.dumps({"kind": "delivery_verdict", "request_id": request["request_id"],
                                      "verdict": value}) + "\n").encode())
            p.stdin.flush()
        try:
            self.assertEqual(frame(), {"kind": "ready"})
            self.assertEqual(post(), 202)
            first = frame()
            self.assertEqual(first["kind"], "arrival")
            manifest = json.loads(Path(first["artifact_manifest_path"]).read_text())
            self.assertEqual(json.loads(Path(manifest["payload_path"]).read_text())["delivery_id"], "process-delivery")
            verdict(first, {"refused": {"reason": "no_binding"}})
            second = frame()
            self.assertEqual(first["artifact_manifest_path"], second["artifact_manifest_path"])
            self.assertEqual(first["evidence"], second["evidence"])
            verdict(second, {"admitted": {"seq": 77}})
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline and not (self.root / "last-admission.json").exists():
                time.sleep(0.02)
            self.assertEqual(post(), 200)
            self.assertEqual(json.loads((self.root / "last-admission.json").read_text())["admitted_seq"], 77)
        finally:
            p.terminate()
            _, err = p.communicate(timeout=5)
        self.assertEqual(p.returncode, 0, err.decode())

    def test_tunnel_failure_restarts_with_backoff(self):
        c = dict(CONFIG, tunnel={"gh": "/fake/gh", "cloudflared": "/fake/cloudflared",
                                "metrics_port": 8788, "register_hook": False})
        class Stop:
            calls = 0
            def is_set(self): return self.calls >= 4
            def wait(self, seconds):
                self.calls += 1
                return self.is_set()
        class Child:
            pid = 123
            def __init__(self): self.stdout = io.BytesIO()
            def poll(self): return 1
        children = []
        def popen(*args, **kwargs):
            child = Child()
            children.append(child)
            return child
        supervisor = TunnelSupervisor(c, self.b, SECRET, popen=popen)
        with patch.object(supervisor, "publish", wraps=supervisor.publish) as publish:
            supervisor.run(Stop())
        self.assertEqual(len(children), 2)
        delays = [call.kwargs["retry_seconds"] for call in publish.call_args_list if "retry_seconds" in call.kwargs]
        self.assertEqual(delays, [1, 2])
        self.assertEqual(json.loads((self.root / "tunnel-status.json").read_text())["state"], "stopped")

    def test_disabled_life_real_ready_without_secret_or_listener(self):
        interface = self.root / "interface"
        interface.mkdir()
        (interface / "interface.json").write_text(json.dumps({
            "interface_driver_config": {"private_config": dict(CONFIG, enabled=False)}}))
        env = environment(self.root)
        env["OCTOPUS_INTERFACE_DIR"] = str(interface)
        p = subprocess.Popen([sys.executable, "-B", str(HERE / "github_issues_life.py"), str(BOUNDARY)],
                             env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            self.assertTrue(select.select([p.stdout], [], [], 5)[0])
            self.assertEqual(json.loads(p.stdout.readline()), {"kind": "ready"})
            self.assertFalse((self.root / "webhook.secret").exists())
            self.assertEqual(json.loads((self.root / "receiver-status.json").read_text()), {"state": "disabled"})
        finally:
            p.terminate()
            p.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
