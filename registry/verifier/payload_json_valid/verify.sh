#!/bin/sh
request=$(cat)
payload_path=$(printf '%s' "$request" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("payload_path",""))' 2>/dev/null)
if [ -z "$payload_path" ] || [ ! -f "$payload_path" ]; then
  printf '{"passed":false,"details":{"handler":"payload_json_valid","error":"payload file unavailable"}}'
  exit 0
fi
if python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$payload_path" 2>/dev/null; then
  printf '{"passed":true,"details":{"handler":"payload_json_valid"}}'
else
  printf '{"passed":false,"details":{"handler":"payload_json_valid","error":"payload is not valid JSON"}}'
fi
