#!/usr/bin/env python3
import fcntl
import importlib.util
import json
import os
import signal
import sys
import threading
from pathlib import Path

from github_issues import Receiver, load_secret, make_server, validate_config
from tunnel import TunnelSupervisor
from relay import RelaySupervisor


def main():
    if len(sys.argv) != 2:
        raise ValueError("expected explicit world_pyecho boundary import path")
    os.umask(0o077)
    path = Path(sys.argv[1])
    sys.path.insert(0, str(path.parent))
    spec = importlib.util.spec_from_file_location("octopus_pyecho_boundary", path)
    boundary = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(boundary)
    boundary.private_directory(boundary.VAR_DIR)
    boundary.private_directory(boundary.PAYLOADS_DIR)
    lock_path = str(Path(boundary.VAR_DIR) / "receiver.lock")
    lock = os.open(lock_path, os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0), 0o600)
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    descriptor = json.loads(boundary.stable_read(
        Path(os.environ["OCTOPUS_INTERFACE_DIR"]) / "interface.json",
        boundary.IO_LIMITS["max_control_frame_bytes"], "interface config"))
    config = validate_config(descriptor["interface_driver_config"]["private_config"])
    stop, fatal = threading.Event(), threading.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_: stop.set())
    status_path = str(Path(boundary.VAR_DIR) / "receiver-status.json")
    if not config["enabled"]:
        boundary.atomic_json(status_path, {"state": "disabled"})
        boundary.signal_ready()
        stop.wait()
        return
    secret = load_secret(boundary)
    receiver = Receiver(config, boundary, secret)
    server = make_server(receiver)
    delivery = threading.Thread(target=receiver.run, args=(stop, fatal), daemon=True)
    tunnel = None
    if "tunnel" in config:
        supervisor = TunnelSupervisor(config, boundary, secret)
        tunnel = threading.Thread(target=supervisor.run, args=(stop,), daemon=True)
    elif "relay" in config:
        supervisor = RelaySupervisor(config, boundary, receiver)
        tunnel = threading.Thread(target=supervisor.run, args=(stop,), daemon=True)
    boundary.atomic_json(status_path, {"state": "listening", "host": "127.0.0.1",
                                      "port": config["port"], "repository": config["repository"]})
    boundary.signal_ready()
    delivery.start()
    if tunnel is not None:
        tunnel.start()
    try:
        while not stop.is_set():
            server.handle_request()
    finally:
        stop.set()
        receiver.wake.set()
        server.server_close()
        if "relay" in config:
            supervisor.close()
        if tunnel is not None:
            tunnel.join(timeout=40)
        delivery.join(timeout=1)
        boundary.atomic_json(status_path, {"state": "failed" if fatal.is_set() else "stopped"})
        if not delivery.is_alive():
            receiver.close()
        os.close(lock)
    if fatal.is_set():
        raise RuntimeError("delivery boundary failed; pending records retained")


if __name__ == "__main__":
    main()
