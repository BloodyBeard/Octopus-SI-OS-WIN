# Time and inactivity triggers

`cron_life` is an inbound interface driver. An activation binds an ordinary
controller run, declares its authoring door, and selects exactly one condition:
`interval_seconds`, `cron`, `due_at`, or `inactivity`. The client projects these
activations in **Triggers**. Timing semantics stay inside the interface.

The descriptor binds `timezone_database_root` to the system timezone database
through `interface_driver_config.host_slots`; clones keep that binding.

An activation that is disabled and bound by no run is a template. Packages ship
templates such as `world_system_review`; Triggers clones one, with private
copies of its controller interfaces and authoring tool, into a live trigger.

An inactivity condition for user-facing tasks is:

```json
{
  "inactivity": {
    "seconds": 3600,
    "interface_match": {
      "interface_type": "world",
      "role": "user",
      "interface_driver_ref": "app_server"
    }
  },
  "fire_on_start": false,
  "timezone": "UTC",
  "reminder_env": "",
  "reminder_default": "Review recent work and make a useful improvement if warranted."
}
```

Every artifact movement with typed run provenance counts, regardless of label.
A run qualifies when one of its checkpoint bindings names an interface whose
descriptor matches every selector field. The optional `within_ee` narrows the
scope to runs whose EE chain contains that EE. The controller, all runs binding
this trigger and every run it created are excluded. Other sensors and passive
background runs therefore do not postpone a user-task quiet period. An
unreadable run record contributes no activity and is named on stderr.

The driver folds `movements.jsonl` once at startup, then reads new complete rows
and reparses run/interface descriptors only when their file metadata changes.
Missing initial activity does not manufacture a deadline. A quiet period that
elapsed while the interface was stopped fires on restart. Polling resolution is
five seconds; the deadline uses the ledger's `recorded_at_unix_ms`.

Each quiet episode delivers one `cron.current_time` artifact with
`schedule_kind: "inactivity"`, its due time, last activity's run and sequence,
`activity_window` (the ledger sequences after the previous delivery through the
last activity) and the configured prompt. `cron-inactivity-state.json` records
preparation and the exact admitted movement. Restart recovers admission even
after a lost acknowledgement. Another delivery requires new qualifying
activity. Activity before the deadline replaces it; once the deadline has
passed the prepared artifact is delivered, and later activity opens the next
episode. Missing admission evidence, a truncated ledger or malformed state
stops the interface explicitly.

`target.mode: "existing_run"` uses the configured receiving frontier.
`target.mode: "new_run"` uses the operator interface's native topology planner
and publishes its plan through the controller's ordinary `file.write` door.
The declared authoring tool must name that exact door. The door projects the
registry roots needed by the plan, including artifact descriptors. Publication
uses `authoring.max_files_per_call` files per call (default 8), with the target
run last. Each batch re-arms the controller through its normal authoring tool;
its immutable request and actual file contents allow interrupted publication
to resume. The plan uses the payload byte limit rather than the control-frame
limit. Private interface clones import their executable owners explicitly and
clone the authoring tool onto their own door.
New interfaces of the created run keep their private state beside this
interface's own; `interface_private_state_root` may be omitted and, when given,
must name that root. The target can explicitly select `model`,
`selected_worker_environment_kinds` and `context_artifact_bindings`; these are
forwarded to the native planner and remain constrained by the worker and EE. Choosing a model does not grant tools.
Role, context and verification requirements remain ordinary artifacts, worker
policy and checkpoint data.

Editing the prompt or duration preserves the consumed activity sequence.
Pausing stops the interface; resuming catches up on the same ledger history.
