#!/usr/bin/env python3
import json
import re
import sys


def main():
    request = json.load(sys.stdin)
    text = request.get("payload_utf8")
    matched = re.match(r"^---\r?\n(.*?)\r?\n---(?:\r?\n|$)", text or "", re.DOTALL)
    fields = {}
    if matched:
        for line in matched.group(1).splitlines():
            if ":" in line:
                key, value = line.split(":", 1)
                fields[key.strip()] = value.strip().strip("\"'")
    passed = bool(fields.get("name") and fields.get("description"))
    print(json.dumps({
        "passed": passed,
        "details": {
            "handler": "context_recipe",
            "artifact": request.get("artifact", {}).get("id"),
            "error": None if passed else "context recipe is not well formed",
        },
    }))


if __name__ == "__main__":
    main()
