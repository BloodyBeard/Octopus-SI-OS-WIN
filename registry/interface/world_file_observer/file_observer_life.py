import importlib.util
import json
import os
import sys
import time
from pathlib import Path

from file_observer import FileObserver, validate_authority


def main():
    if len(sys.argv) != 2:
        raise ValueError("expected the declared world_pyecho Python boundary path")
    boundary_path = Path(sys.argv[1])
    sys.path.insert(0, str(boundary_path.parent))
    spec = importlib.util.spec_from_file_location("octopus_pyecho_boundary", boundary_path)
    boundary = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(boundary)
    os.umask(0o077)
    interface_dir = Path(os.environ["OCTOPUS_INTERFACE_DIR"])
    config = json.loads(boundary.stable_read(interface_dir / "observer.json", boundary.IO_LIMITS["max_control_frame_bytes"], "observer config"))
    life = json.loads(boundary.stable_read(interface_dir / "life.json", boundary.IO_LIMITS["max_control_frame_bytes"], "observer life"))
    validate_authority(life, config)
    for path in (boundary.VAR_DIR, boundary.PAYLOADS_DIR):
        boundary.private_directory(path)
    observer = FileObserver(config, boundary)
    boundary.signal_ready()
    while True:
        observer.poll_once()
        time.sleep(config["poll_interval_ms"] / 1000)


if __name__ == "__main__":
    main()
