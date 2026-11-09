"""Private quick-tunnel lifecycle and exact owned GitHub hook reconciliation."""
import json
import os
import queue
import re
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path
from urllib.parse import urlsplit

ORIGIN = re.compile(r"https://[a-z0-9]+(?:-[a-z0-9]+)*\.trycloudflare\.com(?=[\s/]|$)")


def safe_diagnostic(line, secret):
    text = line.decode("utf-8", "replace")
    if not re.search(r"\b(ERR|WRN|error|fatal|warn|failed|failure)\b", text, re.I):
        return None
    text = text.replace(secret.decode("ascii"), "[secret]")
    text = re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", text)
    text = re.sub(r"(?i)(token|secret|authorization|password|credential)[^\s]*[=: ]+[^\s,;]+",
                  r"\1=[redacted]", text)
    text = re.sub(r"https?://[^\s\"<>]+", "[url]", text)
    text = re.sub(r"(?<![A-Za-z0-9])[A-Za-z0-9_+/=-]{32,}(?![A-Za-z0-9])", "[redacted]", text)
    text = re.sub(r"/(?:Users|home)/[^\s\"<>]+", "[private-path]", text)
    text = "".join(c if 32 <= ord(c) < 127 else " " for c in text).strip()
    return text[:512]


class HookError(RuntimeError):
    pass


class HookManager:
    def __init__(self, config, boundary, secret, run=subprocess.run):
        self.c, self.b, self.secret, self.run = config, boundary, secret, run
        self.path = str(Path(boundary.VAR_DIR) / "hook.json")
        try:
            self.state = json.loads(boundary.stable_read(self.path, 16384, "hook state", private=True))
        except FileNotFoundError:
            self.state = {"repository": config["repository"], "path": config["path"],
                          "id": None, "url": None, "creating": False}
        if self.state["repository"] != config["repository"] or self.state["path"] != config["path"]:
            raise HookError("hook_owner_config_changed")

    def save(self):
        self.b.atomic_json(self.path, self.state)

    def api(self, method, endpoint, payload=None):
        mode = self.c["relay"] if "relay" in self.c else self.c["tunnel"]
        args = [mode["gh"], "api", "--hostname", "github.com",
                "--method", method, endpoint, "-H", "Accept: application/vnd.github+json",
                "-H", "X-GitHub-Api-Version: 2022-11-28"]
        if payload is not None:
            args += ["--input", "-"]
        try:
            response = self.run(args, input=None if payload is None else json.dumps(payload),
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=30, check=False)
        except (OSError, subprocess.TimeoutExpired):
            raise HookError("github_transport_failed") from None
        if response.returncode != 0:
            raise HookError("github_request_failed")
        try:
            return json.loads(response.stdout)
        except (ValueError, TypeError):
            raise HookError("github_invalid_response") from None

    def find_owned(self, base, target):
        matches = []
        for page in range(1, 101):
            hooks = self.api("GET", base + "?per_page=100&page=" + str(page))
            if not isinstance(hooks, list):
                raise HookError("github_invalid_hook_list")
            for hook in hooks:
                if not isinstance(hook, dict):
                    raise HookError("github_invalid_hook")
                url = hook.get("config", {}).get("url", "")
                # Recovery uses the persisted attempted URL; never an arbitrary hook.
                if url == target:
                    matches.append(hook)
            if len(hooks) < 100:
                break
        else:
            raise HookError("github_hook_list_limit")
        if len(matches) > 1:
            raise HookError("github_ambiguous_owned_hooks")
        return matches[0] if matches else None

    def reconcile(self, origin):
        if "relay" in self.c or ORIGIN.fullmatch(origin) is None:
            raise HookError("invalid_tunnel_origin")
        target = origin + self.c["path"]
        return self._reconcile_target(target)

    def reconcile_url(self, target):
        if "relay" not in self.c or target != self.c["relay"]["url"]:
            raise HookError("relay_target_mismatch")
        if re.fullmatch(r"https://smee\.io/[A-Za-z0-9_-]{1,200}", target) is None:
            raise HookError("invalid_relay_target")
        return self._reconcile_target(target)

    def _reconcile_target(self, target):
        base = "repos/" + self.c["repository"] + "/hooks"
        wanted = {"active": True, "events": ["issues"], "config": {
            "url": target, "content_type": "json", "insecure_ssl": "0",
            "secret": self.secret.decode("ascii")}}
        hook_id = self.state["id"]
        if hook_id is None:
            attempted = self.state["url"] or target
            found = self.find_owned(base, attempted)
            if found is not None:
                self.check_id(found)
                self.state.update(id=found["id"], url=attempted, creating=False)
                self.save()
                hook_id = found["id"]
            elif self.state["creating"]:
                # An uncertain POST is not proof of absence, even after one empty list.
                raise HookError("hook_creation_uncertain_requires_reconciliation")
            else:
                self.state.update(url=target, creating=True)
                self.save()
                hook = self.api("POST", base, dict(wanted, name="web"))
                self.check_id(hook)
                self.check_hook(hook, target)
                self.state.update(id=hook["id"], creating=False)
                self.save()
                return hook["id"]
        if type(hook_id) is not int or hook_id <= 0:
            raise HookError("invalid_owned_hook_id")
        endpoint = base + "/" + str(hook_id)
        existing = self.api("GET", endpoint)
        self.check_id(existing)
        if existing["id"] != hook_id or existing.get("config", {}).get("url") not in (self.state["url"], target):
            raise HookError("owned_hook_changed_externally")
        hook = self.api("PATCH", endpoint, wanted)
        self.check_hook(hook, target)
        if hook.get("id") != hook_id:
            raise HookError("github_hook_identity_mismatch")
        self.state.update(url=target, creating=False)
        self.save()
        return hook_id

    @staticmethod
    def check_id(hook):
        if not isinstance(hook, dict) or type(hook.get("id")) is not int or hook["id"] <= 0:
            raise HookError("github_invalid_hook_id")

    @staticmethod
    def check_hook(hook, target):
        if not isinstance(hook, dict):
            raise HookError("github_invalid_hook")
        c = hook.get("config", {})
        if (hook.get("name") != "web" or hook.get("active") is not True
                or hook.get("events") != ["issues"] or c.get("url") != target
                or c.get("content_type") != "json" or str(c.get("insecure_ssl")) != "0"):
            raise HookError("github_hook_response_mismatch")


def cloudflared_ready(port):
    try:
        # Do not route this private health check through an inherited HTTP proxy.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open("http://127.0.0.1:" + str(port) + "/ready", timeout=2) as response:
            return response.status == 200
    except OSError:
        return False


class TunnelSupervisor:
    def __init__(self, config, boundary, secret, popen=subprocess.Popen,
                 ready=cloudflared_ready, hooks=None):
        self.c, self.b, self.popen, self.ready = config, boundary, popen, ready
        self.secret = secret
        self.last_diagnostic = None
        self.diagnostic_bytes = 0
        self.hooks = hooks or HookManager(config, boundary, secret)
        self.child = None
        self.status_path = str(Path(boundary.VAR_DIR) / "tunnel-status.json")
        self.status = {"state": "stopped", "origin": None, "ready": False,
                       "child_pid": None, "child_alive": False, "hook_id": None}

    def publish(self, **changes):
        self.status.update(changes, updated_at=time.time(), last_diagnostic=self.last_diagnostic)
        self.b.atomic_json(self.status_path, self.status)

    def start(self):
        t = self.c["tunnel"]
        self.last_diagnostic = None
        self.child = self.popen(
            [t["cloudflared"], "tunnel", "--url", "http://127.0.0.1:" + str(self.c["port"]),
             "--no-autoupdate", "--metrics", "127.0.0.1:" + str(t["metrics_port"])],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            start_new_session=False)
        origins = queue.Queue(maxsize=1)
        child = self.child

        def read_output():
            try:
                while True:
                    line = child.stdout.readline(8192)
                    if not line:
                        break
                    diagnostic = safe_diagnostic(line, self.secret)
                    if diagnostic and diagnostic != self.last_diagnostic:
                        self.last_diagnostic = diagnostic
                        encoded = (json.dumps({"cloudflared_diagnostic": diagnostic}) + "\n").encode()
                        if self.diagnostic_bytes + len(encoded) <= self.b.IO_LIMITS["max_stderr_bytes"] // 2:
                            sys.stderr.buffer.write(encoded)
                            sys.stderr.buffer.flush()
                            self.diagnostic_bytes += len(encoded)
                    match = ORIGIN.search(line.decode("utf-8", "replace"))
                    if match:
                        try:
                            origins.put_nowait(match.group(0))
                        except queue.Full:
                            pass
            except (OSError, ValueError):
                pass

        self.reader = threading.Thread(target=read_output, daemon=True)
        self.reader.start()
        self.publish(state="starting", origin=None, ready=False, child_pid=self.child.pid,
                     child_alive=True, hook_id=None, error=None)
        return origins

    def stop_child(self):
        child, self.child = self.child, None
        if child is not None:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
            child.stdout.close()
            self.reader.join(timeout=2)

    def run(self, stop):
        backoff = 1
        try:
            while not stop.is_set():
                try:
                    origins = self.start()
                    started = time.monotonic()
                    origin, failures, registered = None, 0, False
                    next_hook_attempt, hook_backoff = 0, 1
                    while not stop.wait(1):
                        if self.child.poll() is not None:
                            raise RuntimeError("cloudflared_exited")
                        try:
                            origin = origins.get_nowait()
                        except queue.Empty:
                            pass
                        healthy = bool(origin) and self.ready(self.c["tunnel"]["metrics_port"])
                        if not healthy:
                            failures += 1
                            self.publish(state="starting" if origin is None else "unhealthy",
                                         origin=origin, ready=False, child_alive=True)
                            if time.monotonic() - started > 60 and failures >= 3:
                                raise RuntimeError("cloudflared_health_failed")
                            continue
                        failures = 0
                        hook_id, error = self.status.get("hook_id"), None
                        if self.c["tunnel"]["register_hook"] and not registered:
                            if time.monotonic() >= next_hook_attempt:
                                try:
                                    hook_id = self.hooks.reconcile(origin)
                                    registered = True
                                except HookError as e:
                                    error = str(e)
                                    next_hook_attempt = time.monotonic() + hook_backoff
                                    hook_backoff = min(60, hook_backoff * 2)
                            else:
                                error = self.status.get("error")
                        self.publish(state="ready" if error is None else "hook_error",
                                     origin=origin, ready=True, child_alive=True,
                                     hook_id=hook_id, error=error)
                        if time.monotonic() - started > 120:
                            backoff = 1
                except Exception as error:
                    self.publish(state="backoff", ready=False, child_alive=False,
                                 error=str(error) if type(error) is RuntimeError else type(error).__name__,
                                 retry_seconds=backoff)
                finally:
                    self.stop_child()
                if stop.wait(backoff):
                    break
                backoff = min(60, backoff * 2)
        finally:
            self.stop_child()
            self.publish(state="stopped", ready=False, child_alive=False, child_pid=None)
