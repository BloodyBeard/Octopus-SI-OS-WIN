import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from authoring import binding_repair_starter


def main():
    parser = argparse.ArgumentParser(description="Print an observer binding-repair example; writes no files.")
    parser.add_argument("--read-root", required=True, help="Explicit existing canonical observation directory")
    options = parser.parse_args()
    files = [{"id": name, "path": f"{options.read_root}/{name}.txt",
              "label": f"file.{name}.observed", "max_bytes": 65536} for name in ("first", "second")]
    example = binding_repair_starter(
        "world_file_repair_example", files, "run_file_repair_producer", "run_file_repair_receiver",
        "system_worker_file_repair_producer", "system_worker_file_repair_relay", "ee_file_repair_example",
        read_roots=[options.read_root])
    print(json.dumps(example, indent=2))


if __name__ == "__main__":
    main()
