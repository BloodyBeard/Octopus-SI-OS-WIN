#!/usr/bin/env python3
import json
import sys

HANDLER = "review_rubric"

VALID_VERDICTS = ("patch is correct", "patch is incorrect")


def _fail(reason, **extra):
    details = {"handler": HANDLER, "error": reason}
    details.update(extra)
    return {"passed": False, "details": details}


def _pass(**extra):
    details = {"handler": HANDLER}
    details.update(extra)
    return {"passed": True, "details": details}


def _load_payload(request):
    text = request.get("payload_utf8")
    if not isinstance(text, str) or not text.strip():
        path = request.get("payload_path")
        if not isinstance(path, str) or not path:
            return None, "review payload is unavailable"
        try:
            with open(path, "r", encoding="utf-8") as handle:
                text = handle.read()
        except Exception as exc:
            return None, f"review payload unreadable: {exc}"
    if not text.strip():
        return None, "review payload is empty"
    try:
        return json.loads(text), None
    except Exception as exc:
        return None, f"review payload is not valid JSON: {exc}"


def _finding_error(idx, finding):
    if not isinstance(finding, dict):
        return f"finding {idx} is not a JSON object"
    title = finding.get("title")
    if not isinstance(title, str) or not title.strip():
        return f"finding {idx} has no non-empty title"
    body = finding.get("body")
    if not isinstance(body, str) or not body.strip():
        return f"finding {idx} has no non-empty body"
    priority = finding.get("priority")
    if priority is not None and priority not in (0, 1, 2, 3):
        return f"finding {idx} priority {priority!r} is outside the 0-3 ladder"
    return None


def main():
    try:
        request = json.load(sys.stdin)
    except Exception as exc:
        print(json.dumps(_fail(f"admission request is not valid JSON: {exc}")))
        return

    if not isinstance(request, dict):
        print(json.dumps(_fail("admission request is not a JSON object")))
        return

    params = request.get("params")
    require_findings = bool(params.get("require_findings")) if isinstance(params, dict) else False

    body, err = _load_payload(request)
    if err is not None:
        print(json.dumps(_fail(err)))
        return

    if not isinstance(body, dict):
        print(json.dumps(_fail("review payload is not a JSON object")))
        return

    findings = body.get("findings")
    if not isinstance(findings, list):
        print(json.dumps(_fail("review payload has no findings array")))
        return

    if require_findings and not findings:
        print(json.dumps(_fail("review requires at least one finding but findings is empty")))
        return

    for idx, finding in enumerate(findings):
        ferr = _finding_error(idx, finding)
        if ferr is not None:
            print(json.dumps(_fail(ferr)))
            return

    verdict = body.get("overall_correctness")
    if verdict not in VALID_VERDICTS:
        print(json.dumps(_fail(
            "overall_correctness is not a valid verdict",
            overall_correctness=verdict,
        )))
        return

    print(json.dumps(_pass(
        finding_count=len(findings),
        overall_correctness=verdict,
    )))


if __name__ == "__main__":
    main()
