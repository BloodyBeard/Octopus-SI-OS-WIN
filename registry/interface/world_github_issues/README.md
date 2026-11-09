# GitHub issue webhook interface

A World sensor, folder identity `world_github_issues`, driver family
`github_webhook`. No kernel, model, client, cron, semantic memory, or issue-list
polling participates in ingress. The package explicitly imports the existing
`world_pyecho/pyecho_life.py` boundary and its content-hash implementation.

## Configuration and activation

Install this complete package and the `world_pyecho` dependency, bind the sensor
to a receiving run, and configure
`interface.json.interface_driver_config.private_config`:

```json
{
  "schema_version": 1,
  "enabled": false,
  "repository": "OWNER/REPOSITORY",
  "port": 8787,
  "path": "/github/replace-with-random-private-route",
  "max_body_bytes": 1048576,
  "max_pending": 1000,
  "retry_seconds": 5,
  "tunnel": {
    "cloudflared": "/absolute/path/to/cloudflared",
    "metrics_port": 8788,
    "register_hook": true,
    "gh": "/absolute/path/to/gh"
  }
}
```

Required: schema_version, enabled, repository, port, path. Unknown keys fail.
Repository is an exact case-sensitive `repository.full_name` match. Port is
1..65535; listener is always 127.0.0.1. Path is exactly a slash followed by
16..200 ASCII letters, digits, underscores, hyphens or slashes; no query string,
normalization or prefix matching. Choose a unique random route per instance.

Optional limits have the defaults above: body 1024..2097152 bytes, pending
1..100000 records, retry 1..300 seconds. The optional tunnel object must contain
all four shown fields. Executable paths are absolute. metrics_port must differ
from port. Omit tunnel for a separately managed ingress; register_hook:false
runs the quick tunnel without GitHub API calls.

The process reads configuration on startup. Change the descriptor through
normal checked authoring so runtime life reconciliation restarts the affected
interface. There is no sidecar config, manual kill, or hidden config watcher.
With enabled:false the process only writes disabled status and emits boundary
ready: no socket, secret generation, tunnel, hook calls or queued-event delivery.
With enabled:true it binds before signaling ready and resumes pending delivery.
A ready frame proves local startup, not public tunnel or GitHub delivery.

## Installation authority

The supplied life.json enables network, Python, and the ledger read handle;
it imports only the explicitly listed package files. For tunnel mode the installer
must additionally declare the exact cloudflared and gh executable paths and
their platform dependencies in life authority. Provide the existing user's
HOME through the life environment (not a source descriptor personal path),
and read access to their gh config directory. macOS gh authentication may
require existing Keychain access and local IPC authority. The default package
does not grant this authority, copy a token, disable sandboxing, or invoke
`gh auth token`. Parent installation must verify actual credential use under
its formed sandbox; source tests do not prove Keychain access.

GitHub credentials need repository webhook read/write permission (and repository
administration rights as required by that account). The child command uses
`gh api --hostname github.com --method ... --input -`; the webhook secret is
passed only in structured JSON stdin. No token is printed, captured from an
authentication command, or stored by this package.

## HTTP and artifacts

Only POST to the configured path is accepted. Content-Type must be
application/json; exactly one Content-Length, signature, event and delivery-id
header is required. Chunked input is rejected. Body length and socket inactivity
are bounded (5 seconds). HMAC SHA-256 covers exact raw body bytes and is compared
with hmac.compare_digest. Authentication precedes parsing and repository checks.

Signed matching-repository ping writes a small private ping.json, returns 200,
and emits no artifact. Other event/action types and PR-shaped issues return 204.
Only issues/opened emits an immutable `github.issue.opened` artifact:

```text
schema_version: 1
delivery_id: GitHub delivery ID
source: {interface, delivery_id, event, action, repository, received_at, body_sha256}
issue: complete GitHub issue object
webhook: complete parsed GitHub event
```

received_at is Unix seconds. Issue content is untrusted external input.
The provider can declare delivery_id as its required input identity and issue
and source as object-valued request context. Route by the exact sensor binding,
with artifact_names:["github.issue.opened"] if filtering is needed.

HTTP 202 `durably_queued` means the immutable artifact and SQLite FULL-synchronous
pending record have been persisted, **not** that the runtime admitted it.
One delivery worker calls the imported boundary's hand_artifact_idempotent;
only its actual admitted sequence sets state=delivered. Refusals remain pending
and retry on the configured interval, including after restart. Protocol failures
stop the life process with pending state retained. A repeated delivery ID cannot
emit a new artifact; different body bytes under that ID return 409.
An already delivered duplicate returns 200. Queue-full and persistence failure
return 503; invalid signatures 401, wrong repo 403, malformed input 400,
wrong path 404, wrong content type 415, oversized input 413.

The boundary's idempotency evidence and ledger lookup recover the crash window
between runtime admission and local receipt commit. Keep the private directory,
artifact files, interface identity and ledger together; deleting this state
removes replay protection. One OS file lock excludes concurrent life instances.
There is no periodic scan of GitHub issues.

## Private state and tunnel lifecycle

All state is under OCTOPUS_VAR_DIR, directory mode 0700, files 0600:

- webhook.secret: generated once (64 random hex characters), or pre-provisioned
  as 32..256 printable ASCII bytes without newline. Never put it in descriptors.
- deliveries.sqlite3: durable pending/delivered protocol records, actual seq,
  attempts, retry deadline, sanitized failure class; no semantic memory.
- payloads/: immutable boundary manifests and payloads.
- last-admission.json: delivery_id, artifact_hash, admitted_seq.
- receiver-status.json: disabled/listening/stopped/failed.
- ping.json: last authenticated ping delivery ID, repository and timestamp.
- hook.json: owned repository, private path, exact attempted/current URL,
  created hook ID, uncertain-creation flag; never the secret or GitHub token.
- tunnel-status.json: origin, child_pid/alive, local tunnel readiness,
  state, hook_id, sanitized error, backoff, last_diagnostic and update time.

The supervisor launches `cloudflared tunnel --url http://127.0.0.1:PORT
--no-autoupdate --metrics 127.0.0.1:METRICS_PORT`. It drains bounded log chunks
without storing raw logs, extracts only an exact HTTPS trycloudflare.com
origin, checks the loopback metrics /ready endpoint and child health, and
restarts failed children with 1..60 second capped exponential backoff.
Error/warning diagnostics are sanitized (secret, credential fields, URLs,
long token-like strings and user-home paths), limited to 512 ASCII characters,
and retained in status. Changed diagnostics are emitted on stderr, capped in
total at half the host stderr budget; stdout remains the boundary protocol.
Startup/health grace is 60 seconds; three unhealthy checks then cause restart.
It terminates and, if necessary, kills its child on normal stop. Children remain
in the host-owned process group for host forced-stop cleanup.

When enabled, hook reconciliation registers only issues, active:true,
content_type:json, insecure_ssl:"0". It checks the actual returned hook data.
After creation it mutates only the saved ID. Before creating, it lists a bounded
100 pages of repository hooks for the exact persisted attempted URL (or the
current target URL on first creation). Ambiguous matches fail. A timed-out or
otherwise uncertain creation is never repeated blindly: discovery must find
that exact hook; otherwise status stays explicit until operator reconciliation.
An externally changed owned-hook URL is refused. No other hooks are deleted.
The saved repository/path cannot be repurposed without explicit state ownership
repair. Hook failures retry with capped backoff without model turns.
tunnel-status.ready reports local tunnel health, not successful hook setup:
also require state=ready and hook_id for managed-hook readiness.

## Verification and limits

Run from the repository:

```sh
python3 -B -m unittest discover -s registry/interface/world_github_issues/tests -v
```

Fourteen local tests cover signature/repository rejection, ping/filtering, exact
HTTP path and size bounds, durable queue capacity, refused admission and restart,
duplicate suppression, full issue details, the real boundary's ledger replay,
a real life subprocess HTTP-to-protocol admission sequence, disabled ready,
owned-hook uncertain POST recovery, non-duplication when still uncertain,
and fake child shutdown/restart/backoff. All GitHub and cloudflared effects are
faked; no live GitHub writes or production process is started by these tests.

Quick Tunnel URLs change and have no SLA. Events sent while this host/tunnel is
offline, while a hook URL is changing, or before ingress durably accepts them
are **not recovered here**. Failed GitHub delivery reconciliation/redelivery is
not implemented. This package guarantees replay of its own accepted queue,
not lossless GitHub catch-up. Public reachability, GitHub authentication,
Keychain sandbox access, hook registration and a real ping require installation
verification. Pending count and individual requests are bounded; retained
delivered receipts/payloads have no automatic garbage collection, so private
disk usage grows. Preserve receipts when managing storage.

Sources: [GitHub repository webhook API](https://docs.github.com/en/rest/repos/webhooks)
and [Cloudflare Quick Tunnels](https://developers.cloudflare.com/tunnel/get-started/quick-tunnels/).
The installed cloudflared tunnel --help was checked for url, no-autoupdate
and metrics options.

## Smee push relay mode

As an alternative to tunnel, set private_config.relay (never both):

```json
{
  "url": "https://smee.io/CHANNEL",
  "gh": "/absolute/path/to/gh",
  "register_hook": false
}
```

All three keys are required. URL must be exactly HTTPS smee.io with one
1..200-character alphanumeric/underscore/hyphen channel; queries, fragments,
credentials, ports and redirects are rejected. Obtain a channel once outside
this process and persist it in the installed descriptor. Existing port/path
settings remain required: the direct loopback receiver stays available.

With register_hook:false no HookManager is instantiated, no hook.json is
read, and no gh executable, authentication or Keychain authority is needed by
the running sensor. The gh field is an unused absolute-path configuration
placeholder in this mode. Register the exact static URL externally using the
same private webhook secret. With register_hook:true the shared HookManager
reconciles only the configured URL after SSE connects, checks the returned
hook, and preserves the same uncertain-create and owned-ID rules as tunnel
mode. It does not rewrite the hook on every reconnect.

relay.py is a declared required package file. The Python HTTPS client opens
one persistent channel GET with Accept:text/event-stream, default verified TLS,
a 10-second connection timeout and 45-second inactivity timeout. Non-200 responses,
redirects, or non-SSE content types fail explicitly. Connection loss reconnects
with capped 1..60-second backoff. This is push subscription, not issue polling.
Shutdown interrupts the retained socket and closes the connection.

SSE lines/events are bounded to 2*max_body_bytes+65536 bytes; only one decoded
event is handled at a time. Named non-message events and heartbeat comments
do not wake a model. Smee message data must be an object with body and the
original signature/event/delivery headers. Headers become a case-insensitive
Message; body is UTF-8 compact JSON with Unicode preserved. Receiver.receive
verifies HMAC on those exact reconstructed bytes. Serialization differences
(including whitespace, number formatting or key ordering) may cause rejection:
**there is no signature bypass or unsigned fallback**.

A 503 from Receiver retains that one event in memory, stops further stream
reads, and retries until durable acceptance or shutdown. This is bounded
backpressure, not a second durable queue. Receiver's 202 persists the event in
the existing queue; its ordinary retry/replay rules then apply. An SSE id only
becomes Last-Event-ID after a successful Receiver result; it is volatile,
not a promise of server replay. Rejected events do not advance that cursor.
If the process stops before Receiver persistence, that event is not recovered
by local protocol state.

relay-status.json privately records connected, ready, state, sanitized error
class, hook_id, last_delivery (HTTP status and receiver outcome), and update
time. ready means the stream connected and optional managed registration
completed, not that GitHub has sent a verified event. last_delivery containing
ping/200 establishes signed ping acceptance; admitted_seq remains in the
existing delivery receipts/last-admission.json.

Smee is a third-party relay and can see the complete payload and headers.
The channel URL is an unguessable capability that must stay private; HMAC
verification remains mandatory even for messages from that channel.
Neither channel secrecy nor SSE provides guaranteed offline catch-up.
This implementation does not promise replay of events that never reached
Receiver's durable queue. No semantic memory or model wake handles transport
maintenance. New local tests cover signed issue and ping forwarding, duplicate
suppression, serialization mismatch rejection, backpressure, SSE bounds,
exact hook URL validation, and fake HTTPS connection shutdown; no remote
calls are made by those tests.
