#!/usr/bin/env python3
import json
import re
import subprocess
import sys


def text_surface(raw):
    try:
        value = json.loads(raw)
    except Exception:
        return raw
    paths = (
        ("ocr_text",), ("recognized_text",), ("text",),
        ("body", "ocr_text"), ("body", "recognized_text"), ("body", "text"),
        ("result", "ocr_text"), ("result", "recognized_text"), ("result", "text"),
        ("body", "result", "text"),
    )
    for path in paths:
        current = value
        for key in path:
            current = current.get(key) if isinstance(current, dict) else None
        if isinstance(current, str):
            return current
    return raw


def main():
    request = json.load(sys.stdin)
    params = request.get("params") if isinstance(request.get("params"), dict) else {}
    expected = params.get("expected_text") or params.get("text")
    source = "text"
    error = None
    haystack = request.get("payload_utf8")
    if not isinstance(expected, str) or not expected.strip():
        error = "missing params.expected_text"
        haystack = ""
    elif isinstance(haystack, str):
        haystack = text_surface(haystack)
    else:
        source = "ocr"
        lang = params.get("lang", "eng")
        if not isinstance(lang, str) or not re.fullmatch(r"[A-Za-z0-9_+]+", lang):
            error = "params.lang must be a tesseract language code"
            haystack = ""
        else:
            try:
                completed = subprocess.run(
                    ["tesseract", request["payload_path"], "stdout", "-l", lang],
                    capture_output=True,
                    text=True,
                    timeout=max(0.001, min(60.0, int(params.get("timeout_ms", 5000)) / 1000)),
                    check=False,
                )
                haystack = completed.stdout
                if completed.returncode != 0:
                    error = completed.stderr.strip() or "ocr engine exited unsuccessfully"
            except Exception as exc:
                error = str(exc)
                haystack = ""
    case_sensitive = params.get("case_sensitive") is True
    if case_sensitive:
        matched = expected in haystack if isinstance(expected, str) else False
    else:
        matched = expected.lower() in haystack.lower() if isinstance(expected, str) else False
    passed = matched and error is None
    print(json.dumps({
        "passed": passed,
        "details": {
            "handler": "ocr_text_contains",
            "expected_text": expected,
            "case_sensitive": case_sensitive,
            "source": source,
            "matched": matched,
            "error": error,
        },
    }))


if __name__ == "__main__":
    main()
