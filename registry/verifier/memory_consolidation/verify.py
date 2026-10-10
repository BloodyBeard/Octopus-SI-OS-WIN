#!/usr/bin/env python3
import json
import os
import sys


LABELS = {
    "memory.fact_sheet",
    "memory.knowledge_graph",
    "memory.ad_hoc_note",
    "memory.summary",
}


def main():
    request = json.load(sys.stdin)
    label = request.get("artifact_label")
    path = request.get("payload_path")
    try:
        with open(path, "rb") as handle:
            payload = handle.read()
    except Exception:
        payload = b""
    passed = label in LABELS and bool(payload)
    if passed and label == "memory.knowledge_graph":
        try:
            value = json.loads(payload)
            passed = isinstance(value, dict) and isinstance(value.get("entities"), list) and isinstance(value.get("relations"), list)
        except Exception:
            passed = False
    print(json.dumps({
        "passed": passed,
        "details": {
            "handler": "memory_consolidation",
            "artifact": request.get("artifact", {}).get("id"),
            "artifact_label": label,
            "payload_exists": isinstance(path, str) and os.path.isfile(path),
            "error": None if passed else "memory artifact is not a valid consolidation result",
        },
    }))


if __name__ == "__main__":
    main()
