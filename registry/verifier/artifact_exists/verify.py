#!/usr/bin/env python3
import json
import os
import sys


def main():
    request = json.load(sys.stdin)
    path = request.get("payload_path")
    passed = isinstance(path, str) and os.path.isfile(path)
    print(json.dumps({
        "passed": passed,
        "details": {
            "handler": "artifact_exists",
            "artifact": request.get("artifact", {}).get("id"),
            "payload_path": path,
            "error": None if passed else "artifact payload is unavailable",
        },
    }))


if __name__ == "__main__":
    main()
