from copy import deepcopy

from file_observer import identifier, validate_config


def observer_package(interface, files, *, read_roots, poll_interval_ms=500):
    config = validate_config({"schema_version": 1, "interface": interface,
                              "poll_interval_ms": poll_interval_ms, "read_roots": read_roots,
                              "files": files}, interface)
    owner = "registry/interface/world_file_observer"
    dependency = "registry/interface/world_pyecho"
    life = {"processes": [{
        "bin": "python3",
        "args": [{"repository_file": f"{owner}/file_observer_life.py"},
                 {"repository_file": f"{dependency}/pyecho_life.py"}],
        "required_files": [f"{owner}/file_observer_life.py", f"{owner}/file_observer.py",
                           f"{dependency}/pyecho_life.py", f"{dependency}/octopus_content_hash.py"],
        "env": {"PYTHONDONTWRITEBYTECODE": "1"},
        "inherit_env": ["PATH"],
        "io_limits": {"max_control_frame_bytes": 1048576, "max_stderr_bytes": 262144,
                      "max_manifest_bytes": 1048576, "max_payload_bytes": 1048576},
        "authority": {
            "read_root_slots": [], "write_root_slots": [], "executable_root_slots": [],
            "read_paths": sorted(read_roots), "write_paths": [],
            "executable_paths": [], "executable_commands": ["python3"],
            "read_env_paths": [], "write_env_paths": [], "executable_env_paths": [],
            "allow_network": False, "allow_local_ipc": False,
        },
        "read_handles": ["ledger"],
    }]}
    prefix = f"registry/interface/{interface}"
    return {
        f"{prefix}/interface.json": {"id": interface, "interface_type": "world", "role": "sensor",
                                     "direction": "inbound", "source_names": ["file.observation"],
                                     "interface_driver_ref": "file_observer_life"},
        f"{prefix}/life.json": life,
        f"{prefix}/observer.json": config,
    }


def wait_starter(interface, files, run, worker, ee, *, read_roots, poll_interval_ms=500):
    identifier(run, "run")
    identifier(worker, "worker")
    identifier(ee, "ee")
    if not worker.startswith("system_worker_"):
        raise ValueError("worker must name an exact system_worker_ pin")
    if worker == interface:
        raise ValueError("observer and worker must be distinct exact interface identities")
    if not files:
        raise ValueError("wait starter requires at least one observed file")
    result = observer_package(interface, files, read_roots=read_roots, poll_interval_ms=poll_interval_ms)
    requirements = [{"mark": f"{interface}.{source['id']}.observed",
                     "artifact_label": source["label"], "verifier": "artifact_exists", "params": {}}
                    for source in files]
    marks = [requirement["mark"] for requirement in requirements]
    result[f"registry/interface/{worker}/interface.json"] = {
        "id": worker, "interface_type": "system", "role": "worker",
        "direction": "bidirectional", "source_names": ["file.observation.wait"],
    }
    result[f"registry/interface/{worker}/policy.json"] = {
        "id": worker, "ceiling": {"interfaces": [interface, worker], "tools": [], "wek": [],
                                   "mark_outcomes": marks, "hard_blocked_tools": []},
        "role_artifacts": [],
    }
    for mark in marks:
        result[f"registry/mark/{mark}/mark.json"] = {"id": mark, "package": interface, "class": "success"}
    result[f"ee_instances/{ee}.json"] = {
        "ee_id": ee, "template_name": "operator_interaction", "parent_ee_id": "ee_existence",
        "goal": "Observe one content snapshot from each declared file through verified artifact arrivals.",
        "config": {}, "policy_narrowing": {"allowed_interfaces": [interface, worker],
                                            "allowed_tools": [], "allowed_worker_environment_kinds": [],
                                            "allowed_mark_outcomes": marks},
    }
    result[f"runs/{run}.json"] = {
        "id": run, "ee": ee, "worker": worker, "trace": f"trace:{run}", "status": "running",
        "interface_state": {}, "active_checkpoints": ["await_files"],
        "checkpoints": [{"id": "await_files", "run": run, "trace": f"trace:{run}:await_files",
                         "epoch": 0, "interface_binding": {"interface": interface,
                                                             "config": {"artifact_policy": {"mode": "all"}}},
                         "outbound_interface_bindings": [], "artifact_bindings": [],
                         "required_verified_artifacts": requirements}],
    }
    return result


def binding_repair_starter(interface, files, producer_run, receiver_run, producer_worker, relay_worker, ee,
                           *, read_roots, poll_interval_ms=500):
    identifier(receiver_run, "receiver_run")
    identifier(relay_worker, "relay_worker")
    if producer_run == receiver_run:
        raise ValueError("producer and receiver runs must be distinct")
    if len({interface, producer_worker, relay_worker}) != 3:
        raise ValueError("observer, producer worker and relay worker must be distinct exact identities")
    if not relay_worker.startswith("system_worker_"):
        raise ValueError("relay_worker must name an exact system_worker_ pin")
    initial = wait_starter(interface, files, producer_run, producer_worker, ee,
                           read_roots=read_roots, poll_interval_ms=poll_interval_ms)
    binding = lambda pin: {"interface": pin, "config": {"artifact_policy": {"mode": "all"}}}
    producer = initial[f"runs/{producer_run}.json"]
    producer["checkpoints"][0]["outbound_interface_bindings"] = [binding(relay_worker)]
    initial[f"registry/interface/{producer_worker}/policy.json"]["ceiling"]["interfaces"].append(relay_worker)
    initial[f"registry/interface/{relay_worker}/interface.json"] = {
        "id": relay_worker, "interface_type": "system", "role": "worker",
        "direction": "bidirectional", "source_names": ["file.observation.relay"],
    }
    marks = initial[f"registry/interface/{producer_worker}/policy.json"]["ceiling"]["mark_outcomes"]
    initial[f"registry/interface/{relay_worker}/policy.json"] = {
        "id": relay_worker, "ceiling": {"interfaces": [producer_worker, relay_worker],
                                        "tools": [], "wek": [], "mark_outcomes": list(marks),
                                        "hard_blocked_tools": []}, "role_artifacts": [],
    }
    scope = initial[f"ee_instances/{ee}.json"]
    scope["goal"] = "Observe declared files, then recover their recorded relay outputs after an exact binding repair."
    scope["policy_narrowing"]["allowed_interfaces"].append(relay_worker)
    receiver = deepcopy(producer)
    receiver.update(id=receiver_run, worker=relay_worker, trace=f"trace:{receiver_run}",
                    active_checkpoints=["await_relay"])
    checkpoint = receiver["checkpoints"][0]
    checkpoint.update(id="await_relay", run=receiver_run, trace=f"trace:{receiver_run}:await_relay",
                      interface_binding=binding(producer_worker), outbound_interface_bindings=[])
    receiver_path = f"runs/{receiver_run}.json"
    initial[receiver_path] = receiver
    repaired = deepcopy(receiver)
    repaired["checkpoints"][0]["interface_binding"] = binding(relay_worker)
    return {
        "initial": initial,
        "repair": {receiver_path: repaired},
        "demonstration": {
            "producer_run": producer_run, "receiver_run": receiver_run, "relay_interface": relay_worker,
            "receiver_generation": {"run": receiver_run, "checkpoint": "await_relay", "epoch": 0},
            "prerequisites": [
                "Explicit read_roots already exist as canonical directories; selected files may be absent.",
                "The target installation supplies world_file_observer, world_pyecho and artifact_exists code.",
                "Its EE chain permits these exact interfaces and marks; installation grants nothing implicitly.",
                "Registry paths are relative to Home; runs and ee_instances paths are relative to its state directory.",
            ],
            "steps": [
                {"action": "author_initial_files", "files": list(initial),
                 "expect": "Both runs are active; the receiver pins the producer worker, which has no outbound addressed to it."},
                {"action": "create_selected_files", "paths": [source["path"] for source in files],
                 "expect": "Inspect the producer for all required marks and durable InterfaceOutbound rows addressed to the relay."},
                {"action": "inspect_receiver_before_repair",
                 "expect": "Its requirements have no matching admitted marks; producer marks cannot satisfy another run."},
                {"action": "author_repair_files", "files": [receiver_path],
                 "expect": "Change only the receiver inbound pin while the runtime stays live; do not resend snapshots or restart."},
                {"action": "inspect_receiver_after_repair",
                 "expect": "Catch-up candidates require current bindings and recorded outbounds. Only exact-generation admitted mark evidence closes requirements."},
            ],
            "evidence": {"required_marks": list(marks), "artifact_labels": [source["label"] for source in files],
                         "relay_source_reference": "invocation_metadata.source_movement_seq",
                         "arrival_alone_is_proof": False, "queued_or_replayed_is_not_assumed": True},
        },
    }
