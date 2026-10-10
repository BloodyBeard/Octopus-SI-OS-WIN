const DAY_MS = 86_400_000;
const { parseProjectionReadRequest } = require("./projectionReadContract.cjs");
const { operatorActivityRows } = require("./operatorActivity.cjs");

const FIXTURE_ROOT_EE_ID = "lab_ee_home";
const FIXTURE_PRIMARY_PROJECT_ID = "lab_project_lumen";
const FIXTURE_RESEARCH_PROJECT_ID = "lab_project_research";
const FIXTURE_OPERATIONS_PROJECT_ID = "lab_project_operations";
const FIXTURE_WORKSPACE_ROOT = "/workspace/lumen";

const FIXTURE_CHAT_ACTIVITY = [
  {
    delay: 320,
    suffix: "turn-started",
    eventKind: "turn_started",
    label: "turn.started",
    text: "",
    eventData: { lifecycle: "started" },
  },
  {
    delay: 780,
    suffix: "reasoning-1",
    eventKind: "reasoning",
    label: "model.reasoning",
    text: "Inspecting the demo workspace",
    eventData: { lifecycle: "completed", summary: [{ text: "Inspecting the demo workspace" }] },
  },
  {
    delay: 1_180,
    suffix: "list-started",
    eventKind: "dynamic_tool",
    label: "tool.call",
    text: "",
    eventData: { arguments: { path: "src" }, status: "started", tool: "coder.list_dir" },
  },
  {
    delay: 1_720,
    suffix: "list-completed",
    eventKind: "dynamic_tool",
    label: "tool.call",
    text: "",
    eventData: { result: { entries: ["components", "styles"] }, status: "completed", tool: "coder.list_dir" },
    callSuffix: "list",
  },
  {
    delay: 2_160,
    suffix: "read-started",
    eventKind: "dynamic_tool",
    label: "tool.call",
    text: "",
    eventData: { arguments: { path: "src/components/ProjectHeader.tsx" }, status: "started", tool: "coder.read_file" },
  },
  {
    delay: 2_760,
    suffix: "read-completed",
    eventKind: "dynamic_tool",
    label: "tool.call",
    text: "",
    eventData: { result: { bytes: 1842 }, status: "completed", tool: "coder.read_file" },
    callSuffix: "read",
  },
  {
    delay: 3_240,
    suffix: "reasoning-2",
    eventKind: "reasoning",
    label: "model.reasoning",
    text: "Preparing a focused change",
    eventData: { lifecycle: "completed", summary: [{ text: "Preparing a focused change" }] },
  },
  {
    delay: 3_760,
    suffix: "change",
    eventKind: "file_change",
    label: "file.change",
    text: "Updated the project header",
    eventData: {
      changes: [{ added: 12, deleted: 3, kind: "update", path: "src/components/ProjectHeader.tsx" }],
      status: "completed",
    },
  },
  {
    delay: 4_280,
    suffix: "command-started",
    eventKind: "dynamic_tool",
    label: "tool.call",
    text: "",
    eventData: { arguments: { command: "npm test -- ProjectHeader" }, status: "started", tool: "coder.build" },
  },
  {
    delay: 5_620,
    suffix: "command-completed",
    eventKind: "dynamic_tool",
    label: "tool.call",
    text: "",
    eventData: { result: { output: "1 test passed" }, status: "completed", tool: "coder.build" },
    callSuffix: "command",
  },
  {
    delay: 6_120,
    suffix: "turn-completed",
    eventKind: "turn_completed",
    label: "turn.completed",
    text: "",
    eventData: { lifecycle: "completed", duration_ms: 5_800 },
  },
];

function freshenScheduledChats(chats) {
  const now = Date.now();
  for (const chat of chats) {
    if (!chat.next_scheduled_at_ms) continue;
    while (chat.next_scheduled_at_ms <= now) chat.next_scheduled_at_ms += DAY_MS;
  }
  return chats;
}

function createFixtureProjectionStore(options = {}) {
  const data = buildFixtureProjection();
  const onChange = typeof options.onChange === "function" ? options.onChange : () => {};
  let generation = 1;
  const pendingChatReplies = new Map();

  async function read(request) {
    const { method, params } = parseProjectionReadRequest(request);
    switch (method) {
      case "projection.runtime.snapshot":
        return { projection: data.projection };
      case "projection.runtime.topology": {
        const includeLedger = params.include_ledger !== false;
        const runs = limitRows(data.runs, { limit: 1_000 });
        const focused = new Set(Array.isArray(params.focused_run_ids) ? params.focused_run_ids.map(String) : []);
        for (const run of runs.slice(0, 12)) focused.add(String(run.run_id));
        const runDetailsRaw = Object.fromEntries([...focused].flatMap((runId) => {
          const detail = data.runDetails[runId];
          return detail ? [[runId, {
            detail,
            trace: includeLedger
              ? { events: limitRows(data.envelopes.filter((row) => row.run_id === runId), { limit: 80 }) }
              : { envelopes: [] },
          }]] : [];
        }));
        return {
          generation,
          snapshotRaw: { projection: data.projection },
          eeRaw: { ee_instances: limitRows(data.eeInstances, { limit: 300 }) },
          runsRaw: { runs, invalid_run_records: [] },
          runCheckpointsRaw: {
            run_checkpoints: runs.map((run) => ({
              run_id: run.run_id,
              checkpoints: Array.isArray(data.runDetails[run.run_id]?.checkpoints)
                ? data.runDetails[run.run_id].checkpoints
                : [],
            })),
          },
          interfacesRaw: { interfaces: data.interfaces },
          bindingsRaw: { bindings: data.bindings },
          artifactsRaw: { artifacts: includeLedger ? limitRows(data.artifacts, { limit: 300 }) : [] },
          marksRaw: { marks: includeLedger ? limitRows(data.marks, { limit: 300 }) : [] },
          workerEnvironmentsRaw: { worker_environments: limitRows(data.workerEnvironments, { limit: 1_000 }) },
          movementsRaw: includeLedger
            ? { movements: limitRows(data.movements, { limit: 1_000 }), total: data.movements.length }
            : { movements: [], total: 0 },
          runDetailsRaw,
        };
      }
      case "projection.ee_instances.list":
        return { ee_instances: limitRows(data.eeInstances, params) };
      case "projection.runs.list":
        return { runs: limitRows(data.runs, params) };
      case "projection.runs.checkpoints.list":
        return {
          run_checkpoints: limitRows(data.runs, params).map((run) => ({
            run_id: run.run_id,
            checkpoints: Array.isArray(data.runDetails[run.run_id]?.checkpoints)
              ? data.runDetails[run.run_id].checkpoints
              : [],
          })),
        };
      case "projection.runs.get": {
        const runId = requiredString(params.run_id, "run_id");
        const detail = data.runDetails[runId];
        if (!detail) throw new Error(`lab run does not exist: ${runId}`);
        return detail;
      }
      case "projection.trace.recent": {
        const runId = requiredString(params.run_id, "run_id");
        return { events: limitRows(data.envelopes.filter((row) => row.run_id === runId), params) };
      }
      case "projection.interfaces.list":
        return { interfaces: limitRows(filterString(data.interfaces, "interface_type", params.interface_type), params) };
      case "projection.interfaces.artifact_transfer.targets":
        return { artifact_transfer_targets: limitRows(filterTargets(data.transferTargets, params), params) };
      case "projection.bindings.list":
        return { bindings: limitRows(data.bindings, params) };
      case "projection.artifacts.list": {
        const source = typeof params.source === "string" ? params.source.trim() : "";
        if (source && !["ledger", "registry"].includes(source)) {
          throw new Error("Test artifact list source must be ledger or registry");
        }
        if (params.previewable_only !== undefined && typeof params.previewable_only !== "boolean") {
          throw new Error("Test artifact list previewable_only must be a boolean");
        }
        const limit = params.limit === undefined ? 1_000 : Number(params.limit);
        if (!Number.isSafeInteger(limit) || limit < 1 || limit > 5_000) {
          throw new Error("Test artifact list limit must be an integer between 1 and 5000");
        }
        const matching = source === "registry" ? [] : filterString(data.artifacts, "label", params.label)
          .map((row) => ({
            ...row,
            previewable: false,
            source: "ledger",
          }));
        const selected = params.previewable_only === true ? [] : matching;
        const artifacts = selected.slice(0, limit);
        return {
          artifacts,
          total: matching.length,
          previewable_total: 0,
          unavailable_total: matching.length,
          returned: artifacts.length,
          limit,
          truncated: selected.length > artifacts.length,
        };
      }
      case "projection.marks.list":
        return { marks: limitRows(data.marks, params) };
      case "projection.worker_environments.list":
        return { worker_environments: limitRows(data.workerEnvironments, params) };
      case "projection.worker_environment_pools.list":
        return { worker_environment_pools: limitRows(data.workerEnvironmentPools, params) };
      case "projection.envelopes.list":
        return { envelopes: limitRows(filterString(data.envelopes, "kind", params.kind), params) };
      case "projection.interfaces.pending.list":
        return { pending_artifacts: limitRows(data.pendingArtifacts, params) };
      case "projection.goals.snapshot":
        return { goals: limitRows(data.goals, params) };
      case "projection.ee_templates.list":
        return { ee_templates: limitRows(data.eeTemplates, params) };
      case "projection.workers.list":
        return { workers: limitRows(data.workers, params) };
      case "projection.tools.list":
        return { tools: limitRows(data.tools, params) };
      case "projection.verifiers.list":
        return { verifiers: limitRows(data.verifiers, params) };
      case "projection.policy.ee.list":
        return { policy: limitRows(data.policy, params) };
      case "projection.descriptors.inventory.list":
        return { descriptors: data.descriptorInventory };
      case "projection.descriptors.kinds.list":
        return { descriptors: data.descriptorKinds };
      case "projection.descriptors.artifact_labels.list":
        return { descriptors: limitRows(data.artifactLabels, params) };
      case "projection.descriptors.mark_kinds.list":
        return { descriptors: limitRows(data.markKinds, params) };
      case "projection.descriptors.tool.list":
        return { descriptors: limitRows(data.tools, params) };
      case "projection.descriptors.worker_environment_kind.list":
        return { descriptors: limitRows(data.workerEnvironmentKinds, params) };
      case "projection.descriptors.ee.list":
        return { descriptors: limitRows(data.eeTemplates, params) };
      case "projection.operator.chats.list": {
        const chats = limitRows(freshenScheduledChats(data.chats), params);
        if (params.include_activity !== true) return { chats };
        const contexts = fixtureChatExecutionContexts(data);
        const activity = [];
        const presented = chats.map((chat) => {
          const worker = contexts.find((context) => context.ee_id === chat.ee_id)?.workers
            .find((worker) => worker.id === chat.worker_choice_interface_id);
          const agent = worker?.client_agent_id ? { id: worker.client_agent_id, name: worker.client_agent_name, icon: worker.client_agent_icon } : null;
          const color = agent ? data.projectPresentations.find((presentation) => presentation.ee_id === chat.ee_id)?.color || null : null;
          const messages = data.chatMessages[chat.run_id] ?? [];
          const deliveries = messages.filter((message) => message.event_kind === "message" && message.terminal === true
            && message.error !== true && message.direction === "out" && message.interface_id === chat.chat_interface_id)
            .map((message) => ({ artifact_id: message.artifact_id, seq: message.movement_seq, movement_id: `fixture:${message.movement_seq}` }));
          const responses = messages.filter((message) => message.event_data?.resolved === true && message.event_data?.request_id)
            .map((message) => ({ key: `${message.event_kind}:${message.event_data.request_id}`, seq: message.movement_seq + 1 }));
          activity.push(...operatorActivityRows({ chat: { ...chat, agent_color: color }, messages, deliveries, responses, agent }).map(fixtureRow));
          return { ...chat, agent_id: agent?.id ?? null, agent_name: agent?.name ?? null, agent_icon: agent?.icon ?? null, agent_color: color };
        });
        return { chats: presented, activity };
      }
      case "projection.operator.chats.status": {
        const runIds = new Set(Array.isArray(params.run_ids) ? params.run_ids.map(String) : []);
        return {
          chats: freshenScheduledChats(data.chats)
            .filter((chat) => runIds.has(String(chat.run_id)))
            .map((chat) => ({
              run_id: chat.run_id,
              turn_open: chat.turn_open === true,
              turn_state: chat.turn_state,
              updated_at_ms: chat.updated_at_ms,
              last_movement_seq: chat.last_movement_seq,
            })),
          ledger_cursor_seq: data.chats.reduce(
            (latest, chat) => Math.max(latest, Number(chat.last_movement_seq) || 0),
            0,
          ),
        };
      }
      case "projection.operator.os.scope": {
        const home = data.chats.find((chat) => chat.client_surface_role === "home") ?? null;
        const rootEnvironment = data.eeInstances.find((environment) => environment.ee_id === FIXTURE_ROOT_EE_ID) ?? null;
        const projects = data.eeInstances.filter((environment) => environment.parent_ee_id === FIXTURE_ROOT_EE_ID);
        return {
          status: home ? "ready" : "awaiting_home",
          root_ee_id: FIXTURE_ROOT_EE_ID,
          home_run_id: String(home?.run_id || ""),
          home_interface_id: String(home?.chat_interface_id || ""),
          root_environment: rootEnvironment,
          projects,
          environments: data.eeInstances,
          ee_templates: data.eeTemplates,
          project_presentations: data.projectPresentations,
          task_authoring_profiles: fixtureTaskAuthoringProfiles(data.eeInstances),
          client_interface_ids: data.chats.map((chat) => chat.chat_interface_id),
          client_runs: data.chats.map((chat) => ({
            run_id: chat.run_id,
            ee_id: chat.ee_id,
            interface_id: chat.chat_interface_id,
            surface_role: chat.client_surface_role,
            provider_choice_interface: chat.provider_interface_id,
            worker_choice_interface: chat.worker_choice_interface_id,
          })),
          read_only_runs: [],
          errors: [],
        };
      }
      case "projection.operator.os.workspace_setup":
        return { status: "not_configured", receipt: null, errors: [] };
      case "projection.operator.os.applications":
        return {
          status: "ready",
          root_ee_id: FIXTURE_ROOT_EE_ID,
          applications: data.applications,
          projects: data.eeInstances.map(fixtureApplicationProject),
          errors: [],
        };
      case "projection.operator.os.drivers":
        return {
          status: "ready",
          root_ee_id: FIXTURE_ROOT_EE_ID,
          host_root_refs: data.hostRootRefs,
          host_manifest_ready: true,
          drivers: data.drivers,
          projects: data.eeInstances.map(fixtureApplicationProject),
          errors: [],
        };
      case "projection.operator.run.activity": {
        const runId = requiredString(params.run_id, "run_id");
        return fixtureRunActivityPage(runId, data.chatMessages[runId] || [], params);
      }
      case "projection.operator.chat.contexts":
        return {
          contexts: fixtureChatExecutionContexts(data),
          application_catalog: data.applications.map((application) => ({
            id: application.id,
            name: application.name,
            description: application.description,
            category: application.category,
            icon: application.icon,
            logo_data_url: application.logo_data_url,
            logo_dark_data_url: application.logo_dark_data_url,
            interfaces: application.interfaces,
            artifact_bindings: application.artifact_bindings,
          })),
        };
      case "projection.operator.chat.messages":
        return { messages: data.chatMessages[requiredString(params.run_id, "run_id")] || [] };
      case "projection.operator.providers.status":
        return { providers: fixtureProviderStatus(data.chats) };
      case "projection.movements.list":
        return fixtureMovementPage(data.movements, params);
      case "projection.movements.payload":
        return { resolved: false, blob_path: String(params.blob_path || ""), size: 0, text: "", truncated: false };
      case "projection.client_surfaces.list":
        return { generation, root: "artifact://lab", documents: [], errors: [] };
      case "projection.interface_artifacts.list":
        return { artifacts: [] };
      default:
        throw new Error(`lab projection method is not implemented: ${method}`);
    }
  }

  async function submitChatTurn(request) {
    const text = requiredString(request?.text, "text");
    const providerInterfaceId = requiredString(request?.providerInterfaceId, "providerInterfaceId");
    if (!fixtureProviderInterfaceDescriptors().some((provider) => provider.id === providerInterfaceId)) {
      throw new Error(`Test provider does not exist: ${providerInterfaceId}`);
    }
    const now = Date.now();
    const runId = typeof request?.runId === "string" && request.runId.trim()
      ? request.runId.trim()
      : `lab_run_${now.toString(36)}`;
    let chat = data.chats.find((item) => item.run_id === runId);
    if (!chat) {
      const chatInterfaceId = `lab_world_operator_chat_${runId}`;
      chat = fixtureRow({
        run_id: runId,
        title: text.slice(0, 72),
        chat_interface_id: chatInterfaceId,
        client_surface_role: request?.clientSurfaceRole === "home" ? "home" : "task",
        writable: true,
        os_root_ee_id: FIXTURE_ROOT_EE_ID,
        turn_open: false,
        ee_id: String(request?.eeId || data.eeInstances[0]?.ee_id || ""),
        ee_goal: String(data.eeInstances.find((environment) => environment.ee_id === request?.eeId)?.goal || ""),
        ee_template_name: String(data.eeInstances.find((environment) => environment.ee_id === request?.eeId)?.template_name || ""),
        worker_interface_id: String(request?.workerInterfaceId || data.workers[0]?.worker || ""),
        worker_choice_interface_id: String(request?.workerInterfaceId || data.workers[0]?.worker || ""),
        door_host_slots: request?.doorHostSlots || {},
        provider_interface_id: providerInterfaceId,
        provider_bound_interface_id: `${providerInterfaceId}_${runId}`,
        provider_model: String(request?.providerModel || "gpt-5.6-codex"),
        provider_models_refreshable: false,
        reasoning_effort: String(request?.reasoningEffort || "medium"),
        created_at_ms: now,
        updated_at_ms: now,
        message_count: 0,
        pinned_artifact_ids: [],
        continued_from: request?.continuation || null,
      });
      data.chats.unshift(chat);
      data.chatMessages[runId] = [];
    }
    if (chat.turn_open === true || pendingChatReplies.has(runId)) {
      throw new Error(`Test run '${runId}' already has an in-flight reply; queue or steer the message instead.`);
    }
    const messages = data.chatMessages[runId];
    const seq = nextFixtureChatSeq(data);
    const messageId = String(request?.messageId || `lab_message_${now.toString(36)}`);
    messages.push(fixtureRow({
      id: messageId,
      run_id: runId,
      role: "user",
      label: "operator.message",
      direction: "in",
      text,
      event_kind: "message",
      event_data: { text, message_id: messageId },
      created_at_ms: now,
      movement_seq: seq,
      artifact_id: messageId,
      content_hash: `lab${messageId}`,
      interface_id: chat.chat_interface_id,
      attachments: fixtureChatAttachments(request?.attachments),
      error: false,
      terminal: true,
    }));
    chat.turn_open = true;
    chat.updated_at_ms = now;
    chat.message_count = messages.length;
    generation += 1;
    onChange(generation);
    const scheduleActivity = (activity) => {
      const timer = setTimeout(() => {
        if (!pendingChatReplies.has(runId)) return;
        const createdAtMs = Date.now();
        const callSuffix = activity.callSuffix
          || activity.suffix.split("-")[0];
        messages.push(fixtureRow({
          id: `${messageId}:${activity.suffix}`,
          run_id: runId,
          role: "assistant",
          label: activity.label,
          direction: "out",
          event_kind: activity.eventKind,
          event_data: {
            ...activity.eventData,
            ...(activity.eventKind === "dynamic_tool" ? { call_id: `${messageId}:${callSuffix}` } : {}),
            ...(activity.eventKind === "turn_started" || activity.eventKind === "turn_completed"
              ? { turn_id: `${messageId}:turn` }
              : {}),
            simulated: true,
          },
          text: activity.text,
          created_at_ms: createdAtMs,
          movement_seq: nextFixtureChatSeq(data),
          artifact_id: `${messageId}:${activity.suffix}`,
          content_hash: `lab${messageId}${activity.suffix}`,
          interface_id: chat.provider_interface_id,
          attachments: [],
          error: false,
          terminal: false,
        }));
        chat.updated_at_ms = createdAtMs;
        chat.message_count = messages.length;
        generation += 1;
        onChange(generation);
      }, activity.delay);
      timer.unref?.();
    };
    for (const activity of FIXTURE_CHAT_ACTIVITY) scheduleActivity(activity);
    const replyTimer = setTimeout(() => {
      pendingChatReplies.delete(runId);
      const steers = messages.filter((message) => message.event_kind === "steering" && message.movement_seq > seq);
      const replyAt = Date.now();
      messages.push(fixtureRow({
        id: `${messageId}:reply`,
        run_id: runId,
        role: "assistant",
        label: "interface.artifact.output",
        direction: "out",
        event_kind: "message",
        event_data: { simulated: true, steering_messages: steers.length },
        text: steers.length
          ? `I incorporated ${steers.length} in-flight ${steers.length === 1 ? "message" : "messages"} and completed the demo change.`
          : "Done. I inspected the demo workspace, updated `src/components/ProjectHeader.tsx`, and the focused check passes.",
        created_at_ms: replyAt,
        movement_seq: nextFixtureChatSeq(data),
        artifact_id: `${messageId}:reply`,
        content_hash: `lab${messageId}reply`,
        interface_id: chat.chat_interface_id,
        attachments: [],
        error: false,
        terminal: true,
      }));
      chat.turn_open = false;
      chat.updated_at_ms = replyAt;
      chat.message_count = messages.length;
      generation += 1;
      onChange(generation);
    }, 6_520);
    replyTimer.unref?.();
    pendingChatReplies.set(runId, replyTimer);
    return {
      runId,
      chatInterfaceId: chat.chat_interface_id,
      modelInterfaceId: providerInterfaceId,
      workerInterfaceId: chat.worker_interface_id,
      clientSurfaceRole: chat.client_surface_role,
      label: "operator.message",
      messageId,
      payloadPath: `lab://artifacts/${messageId}`,
      contentHash: `lab${messageId}`,
      attachmentCount: Array.isArray(request?.attachments) ? request.attachments.length : 0,
      clientWrites: [],
      messageRecovered: false,
      messageVerdict: { admitted: { seq } },
      outboundCursor: { offset: 0, seq: null },
    };
  }

  async function submitChatSteer(request) {
    const runId = requiredString(request?.runId, "runId");
    const text = typeof request?.text === "string" ? request.text.trim() : "";
    const attachments = fixtureChatAttachments(request?.attachments);
    if (!text && attachments.length === 0) throw new Error("Test steering message must carry text or an attachment.");
    const chat = data.chats.find((item) => item.run_id === runId);
    if (!chat) throw new Error(`Test chat does not exist: ${runId}`);
    if (chat.turn_open !== true || !pendingChatReplies.has(runId)) {
      throw new Error(`Test run '${runId}' has no in-flight reply to steer.`);
    }
    const interfaceId = chat.chat_interface_id;
    const label = "operator.message";
    const messageId = String(request?.messageId || `lab_steer_${Date.now().toString(36)}`);
    const seq = nextFixtureChatSeq(data);
    const createdAtMs = Date.now();
    data.chatMessages[runId].push(fixtureRow({
      id: messageId,
      run_id: runId,
      role: "user",
      label,
      direction: "in",
      event_kind: "steering",
      event_data: { text, message_id: messageId, steering_status: "accepted" },
      text,
      created_at_ms: createdAtMs,
      movement_seq: seq,
      artifact_id: messageId,
      content_hash: `lab${messageId}`,
      interface_id: interfaceId,
      attachments,
      error: false,
      terminal: true,
    }));
    chat.updated_at_ms = createdAtMs;
    chat.message_count = data.chatMessages[runId].length;
    generation += 1;
    onChange(generation);
    return {
      interfaceId,
      runId,
      label,
      messageId,
      attachmentCount: attachments.length,
      contentHash: `lab${messageId}`,
      messageRecovered: false,
      verdict: { admitted: { seq } },
    };
  }

  async function updateChatPins(request) {
    const runId = requiredString(request?.runId, "runId");
    const artifactId = requiredString(request?.artifactId, "artifactId");
    if (typeof request?.pinned !== "boolean") throw new Error("pinned must be a boolean");
    const chat = data.chats.find((item) => item.run_id === runId);
    if (!chat) throw new Error(`Test chat does not exist: ${runId}`);
    const messages = data.chatMessages[runId] || [];
    const source = messages.find((message) => message.artifact_id === artifactId);
    const currentItems = Array.isArray(chat.pinned_context_items) ? chat.pinned_context_items : [];
    if (!source && !currentItems.some((item) => item.artifact_id === artifactId)) {
      throw new Error(`Test chat artifact does not exist: ${artifactId}`);
    }
    const current = Array.isArray(chat.pinned_artifact_ids) ? chat.pinned_artifact_ids : [];
    const pinnedId = request.pinned && request.excerpt
      ? `operator.pinned_excerpt:lab-${artifactId}-${request.excerpt.start}-${request.excerpt.end}`
      : artifactId;
    chat.pinned_artifact_ids = request.pinned
      ? Array.from(new Set([...current, pinnedId]))
      : current.filter((id) => id !== artifactId);
    chat.pinned_context_items = request.pinned && request.excerpt
      ? [...currentItems, {
          artifact_id: pinnedId,
          source_artifact_id: artifactId,
          source_message_id: request.excerpt.sourceMessageId,
          text: request.excerpt.text,
          start: request.excerpt.start,
          end: request.excerpt.end,
          role: request.excerpt.role,
        }]
      : currentItems.filter((item) => item.artifact_id !== artifactId);
    generation += 1;
    onChange(generation);
    return {
      runId,
      pinnedArtifactIds: chat.pinned_artifact_ids,
      pinnedContextItems: chat.pinned_context_items.map((item) => ({
        artifactId: item.artifact_id,
        sourceArtifactId: item.source_artifact_id,
        sourceMessageId: item.source_message_id,
        text: item.text,
        start: item.start,
        end: item.end,
        role: item.role,
      })),
      clientWrites: [],
    };
  }

  return {
    read,
    submitChatTurn,
    submitChatSteer,
    updateChatPins,
    generation: () => generation,
    invalidate: () => {},
    ensureWatchers: () => {},
  };
}

function nextFixtureChatSeq(data) {
  let maximum = 0;
  for (const rows of Object.values(data.chatMessages)) {
    for (const row of rows) maximum = Math.max(maximum, Number(row.movement_seq) || 0);
  }
  return maximum + 1;
}

function fixtureChatAttachments(value) {
  return (Array.isArray(value) ? value : []).map((input, index) => ({
    id: String(input?.id || `${String(input?.name || "attachment")}:${index}`),
    name: String(input?.name || `Attachment ${index + 1}`),
    size: Number(input?.size) || Number(input?.bytes?.byteLength) || 0,
    type: String(input?.type || "application/octet-stream"),
    label: String(input?.label || "operator.file"),
    last_modified: Number(input?.lastModified) || 0,
  }));
}

function buildFixtureProjection() {
  const now = Date.now();
  const at = (days, hours = 0) => new Date(now - days * DAY_MS - hours * 3_600_000).toISOString();
  const interfaces = fixtureInterfaces();
  const eeInstances = fixtureEnvironments(at);
  const workerEnvironments = fixtureWorkerEnvironments(eeInstances);
  const runs = fixtureRuns(at, eeInstances, workerEnvironments);
  for (const [index, environment] of workerEnvironments.entries()) {
    const run = runs[index];
    environment.run_id = run?.run_id || "";
    environment.worker = run?.worker || "";
    environment.lifecycle = environment.status === "released" ? "released" : "active";
    environment.last_activity_movement_seq = 1_000 - index;
  }
  const runDetails = {};
  const bindings = [];
  for (const [index, run] of runs.entries()) {
    const checkpoints = fixtureCheckpoints(run, index, at);
    const runBindings = checkpoints.flatMap((checkpoint) => [
      fixtureRow({
        binding_id: `${checkpoint.checkpoint_id}:in`,
        run_id: run.run_id,
        ee_id: run.ee_id,
        worker: run.worker,
        checkpoint_id: checkpoint.checkpoint_id,
        interface_id: checkpoint.interface,
        direction: "inbound",
        status: checkpoint.status,
      }),
      ...checkpoint.out_interfaces.map((interfaceId) => fixtureRow({
        binding_id: `${checkpoint.checkpoint_id}:out:${interfaceId}`,
        run_id: run.run_id,
        ee_id: run.ee_id,
        worker: run.worker,
        checkpoint_id: checkpoint.checkpoint_id,
        interface_id: interfaceId,
        direction: "outbound",
        status: checkpoint.status,
      })),
    ]);
    bindings.push(...runBindings);
    runDetails[run.run_id] = fixtureRow({
      run,
      bindings: runBindings,
      checkpoints,
      worker_environment: workerEnvironments.find((item) => item.worker_environment_id === run.worker_environment_id),
      artifacts: [],
      marks: [],
    });
  }

  const artifacts = Array.from({ length: 58 }, (_, index) => {
    const run = runs[index % runs.length];
    const labels = ["context.reference", "llm.request", "interface.episode.output", "worker.native.summary", "run.start", "operator.message"];
    return fixtureRow({
      artifact_id: `lab_artifact_${String(index + 1).padStart(3, "0")}`,
      label: labels[index % labels.length],
      route_label: index % 4 === 0 ? "system.worker" : "",
      run_id: run.run_id,
      ee_id: run.ee_id,
      worker: run.worker,
      status: index % 13 === 0 ? "pending" : "recorded",
      created_at: at((index * 7) % 30, index % 18),
      size: 4_096 + (index % 11) * 3_277,
    });
  }).sort((left, right) => Date.parse(right.created_at) - Date.parse(left.created_at));

  const marks = Array.from({ length: 18 }, (_, index) => {
    const run = runs[index % runs.length];
    return fixtureRow({
      movement_id: `lab_mark_movement_${index + 1}`,
      mark_name: ["llm.output.ok", "artifact.admitted", "run.checkpoint.closed"][index % 3],
      status: "admitted",
      run_id: run.run_id,
      ee_id: run.ee_id,
      worker: run.worker,
      checkpoint_id: `${run.run_id}:checkpoint:${(index % 2) + 1}`,
      artifact_id: artifacts[index % artifacts.length].artifact_id,
      created_at: at((index * 5) % 29, index % 12),
    });
  }).sort((left, right) => Date.parse(right.created_at) - Date.parse(left.created_at));

  const movements = fixtureMovements(now, runs, interfaces, artifacts);
  const envelopes = movements.slice(0, 180).map((movement, index) => fixtureRow({
    envelope_id: `lab_envelope_${String(index + 1).padStart(3, "0")}`,
    kind: ["InterfaceArrival", "ArtifactRecorded", "InterfaceOutbound", "MarkOutcomeAdmitted(llm.output.ok)"][index % 4],
    run_id: movement.raw_run_id,
    trace_id: movement.trace,
    ts: new Date(movement.recorded_at_unix_ms).toISOString(),
    recorded_at_unix_ms: movement.recorded_at_unix_ms,
    invocation_metadata: {
      runtime: {
        run: movement.raw_run_id,
        ee: movement.raw_ee_id,
        worker: movement.raw_worker,
        checkpoint: `${movement.raw_run_id}:checkpoint:${(index % 2) + 1}`,
      },
    },
    artifact_id: movement.artifact_id,
    artifact: { artifact_id: movement.artifact_id, label: movement.artifact_label },
  }));

  for (const detail of Object.values(runDetails)) {
    detail.artifacts = artifacts.filter((artifact) => artifact.run_id === detail.run.run_id);
    detail.marks = marks.filter((mark) => mark.run_id === detail.run.run_id);
  }

  const goals = eeInstances.map((environment, index) => fixtureRow({
    goal_id: `lab_goal_${index + 1}`,
    status: environment.status === "done" ? "complete" : "active",
    kind: environment.template_name,
    title: environment.goal,
    ee_id: environment.ee_id,
    created_at: environment.created_at,
  }));
  const eeTemplates = eeInstances.map((environment) => fixtureRow({
    descriptor_id: `ee_template:${environment.template_name}`,
    id: environment.template_name,
    name: environment.template_name,
    goal: environment.goal,
    source: "lab.registry",
    allowed_interfaces: environment.policy.allowed_interfaces,
    allowed_tools: environment.policy.allowed_tools,
    allowed_worker_environment_kinds: environment.policy.allowed_worker_environment_kinds,
  }));
  const tools = [
    "coder.list_dir",
    "coder.read_file",
    "coder.grep",
    "coder.apply_patch",
    "coder.build",
    "browser.navigate",
    "artifact_handoff",
  ].map((name, index) => fixtureRow({
    descriptor_id: `tool:${name}`,
    id: name,
    name,
    interface_ref: interfaces[index % interfaces.length].rule_id,
    effect_categories: index < 3 ? ["read"] : ["world_effect"],
    is_destructive: name === "coder.apply_patch" || name === "coder.build",
    supports_progress: name.startsWith("coder."),
  }));
  const verifiers = ["artifact_schema", "policy_grant", "mark_admission"].map((name) => fixtureRow({
    name,
    handler_name: `verify_${name}`,
    source: "lab.registry",
  }));
  const workers = interfaces.filter((item) => item.interface_type === "System" && item.role === "worker").map((item, index) => fixtureRow({
    worker: item.rule_id,
    status: "registered",
    ceiling_interfaces_count: 4 + index,
    ceiling_tools_count: 3 + index,
    ceiling_worker_environment_kinds_count: 2,
    ceiling_mark_outcomes_count: 3,
    policy_path: `lab://registry/interface/${item.rule_id}/worker_policy.json`,
  }));
  const policy = eeTemplates.map((template) => fixtureRow({
    name: template.name,
    allowed_interfaces_count: template.allowed_interfaces.length,
    allowed_tools_count: template.allowed_tools.length,
    allowed_worker_environment_kinds_count: template.allowed_worker_environment_kinds.length,
    source: "lab.registry",
  }));
  const artifactLabels = [...new Set(artifacts.map((item) => item.label))].map((label) => fixtureRow({
    label,
    lifecycle: "artifact",
    allowed_writers: ["lab_system_worker_planner"],
    finalize_recommended: label.includes("output"),
  }));
  const markKinds = [...new Set(marks.map((item) => item.mark_name))].map((name) => fixtureRow({
    name,
    lifecycle: "registered",
    class: "checkpoint",
    package: "lab.verifiers",
  }));
  const descriptorKinds = ["interface", "ee_template", "worker_environment_kind", "tool", "verifier", "artifact", "mark"].map((kind) => fixtureRow({
    kind,
    identity_field: kind === "mark" ? "name | id | kind" : "id | name",
    descriptor_filename: `${kind}.json`,
    source: "lab.projection",
  }));
  const descriptorInventory = descriptorKinds.map((row, index) => fixtureRow({
    registry: row.kind,
    count: [interfaces.length, eeTemplates.length, workerEnvironments.length, tools.length, verifiers.length, artifactLabels.length, markKinds.length][index],
    runtime_count: index === 0 ? 2 : 0,
    warning_count: 0,
    descriptor_filename: row.descriptor_filename,
  }));
  const runtimeDescriptors = [fixtureRow({ descriptor_id: "lab_runtime_voice_widget", kind: "client_surface", name: "voice_status", created_by_ee: eeInstances[0].ee_id, accepted_at: at(1) })];
  const descriptorMutations = [fixtureRow({ mutation_id: "lab_mutation_1", status: "accepted", kind: "client_surface", name: "voice_status", descriptor_id: runtimeDescriptors[0].descriptor_id })];
  const transferTargets = bindings.filter((item) => item.direction === "inbound").map((binding) => fixtureRow({
    target_id: `${binding.interface_id}:${binding.run_id}:${binding.checkpoint_id}`,
    interface_id: binding.interface_id,
    status: "ready",
    run_id: binding.run_id,
    ee_id: binding.ee_id,
    worker: binding.worker,
    checkpoint_id: binding.checkpoint_id,
    reason: "lab checkpoint target",
  }));
  const pendingArtifacts = artifacts.filter((item) => item.status === "pending").map((artifact) => fixtureRow({
    pending_id: `pending:${artifact.artifact_id}`,
    template_name: "lab_world_operator_chat",
    artifact_id: artifact.artifact_id,
    trace_id: `lab_trace_${artifact.artifact_id}`,
    created_at: artifact.created_at,
  }));
  const workerEnvironmentPools = [fixtureRow({ pool_id: "lab_pool_local", worker_environment_kind: "git_worktree", status: "active", allocation_policy: "per_run", worker_environment_count: 3 })];
  const workerEnvironmentKinds = Array.from(new Map(workerEnvironments.map((environment) => [environment.worker_environment_kind, fixtureRow({
    id: environment.worker_environment_kind,
    name: environment.worker_environment_kind,
    worker_environment_kind_id: environment.worker_environment_kind,
    worker_environment_kind: environment.worker_environment_kind,
    substrate_family: environment.substrate_family,
    capabilities: environment.capabilities,
    supported_executor_kinds: environment.substrate_family === "browser" ? ["browser"] : ["exec"],
    boundary_defaults: { network_ceiling: environment.substrate_family === "filesystem" ? "denied" : "restricted" },
    lifecycle: { requires_release: environment.worker_environment_kind.includes("session") },
    descriptor_path: environment.descriptor_path,
    source: "lab.registry",
  })])).values());
  const projectPresentations = [
    fixtureRow({ ee_id: FIXTURE_PRIMARY_PROJECT_ID, name: "Lumen", icon: "code", color: "violet" }),
    fixtureRow({ ee_id: FIXTURE_RESEARCH_PROJECT_ID, name: "Research", icon: "globe", color: "teal" }),
    fixtureRow({ ee_id: FIXTURE_OPERATIONS_PROJECT_ID, name: "Operations", icon: "briefcase", color: "orange" }),
  ];
  const applications = fixtureApplications(eeInstances);
  const drivers = fixtureDrivers(eeInstances, workerEnvironments);
  const hostRootRefs = {
    host_filesystem_root: "/",
    workspace_root: FIXTURE_WORKSPACE_ROOT,
    browser_profile_root: "/workspace/.browser-profile",
  };
  const scheduledArrival = new Date(now + 2 * 60 * 60 * 1_000);
  const scheduledFollowup = new Date(scheduledArrival.getTime() + 2 * 60 * 60 * 1_000);
  const chatSeeds = [
    { runId: "lab_run_1", title: "Home", surfaceRole: "home", eeId: FIXTURE_ROOT_EE_ID, worker: "lab_system_worker_planner", updatedDaysAgo: 0.08, createdDaysAgo: 18, nextScheduledAtMs: 0 },
    { runId: "lab_run_2", title: "Add connection status to project header", surfaceRole: "task", eeId: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", updatedDaysAgo: 0.03, createdDaysAgo: 2, nextScheduledAtMs: 0 },
    { runId: "lab_run_3", title: "Review onboarding empty states", surfaceRole: "task", eeId: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", updatedDaysAgo: 0.4, createdDaysAgo: 3, nextScheduledAtMs: 0 },
    { runId: "lab_run_4", title: "Prepare weekly project pulse", surfaceRole: "task", eeId: FIXTURE_OPERATIONS_PROJECT_ID, worker: "lab_system_worker_planner", updatedDaysAgo: 0.9, createdDaysAgo: 6, nextScheduledAtMs: scheduledArrival.getTime() },
    { runId: "lab_run_5", title: "Compare onboarding patterns", surfaceRole: "task", eeId: FIXTURE_RESEARCH_PROJECT_ID, worker: "lab_system_worker_research", updatedDaysAgo: 1.4, createdDaysAgo: 5, nextScheduledAtMs: scheduledFollowup.getTime() },
    { runId: "lab_run_6", title: "Polish settings navigation", surfaceRole: "task", eeId: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", updatedDaysAgo: 2.1, createdDaysAgo: 8, nextScheduledAtMs: 0 },
    { runId: "lab_run_7", title: "Prepare release notes", surfaceRole: "task", eeId: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", updatedDaysAgo: 3.2, createdDaysAgo: 9, nextScheduledAtMs: 0 },
    { runId: "lab_run_8", title: "Investigate preview reconnect", surfaceRole: "task", eeId: FIXTURE_PRIMARY_PROJECT_ID, worker: "lab_system_worker_coder", updatedDaysAgo: 4.1, createdDaysAgo: 11, nextScheduledAtMs: 0 },
  ];
  const chats = chatSeeds.map((seed, index) => {
    const chatEnvironment = eeInstances.find((environment) => environment.ee_id === seed.eeId) ?? eeInstances[0];
    return fixtureRow({
      run_id: seed.runId,
      title: seed.title,
      chat_interface_id: `lab_world_operator_chat_${seed.runId}`,
      client_surface_role: seed.surfaceRole,
      writable: true,
      os_root_ee_id: FIXTURE_ROOT_EE_ID,
      turn_open: false,
      turn_state: "idle",
      ee_id: chatEnvironment.ee_id,
      ee_goal: chatEnvironment.goal,
      ee_template_name: chatEnvironment.template_name,
      provider_interface_id: "lab_world_provider_codex",
      provider_bound_interface_id: `lab_world_provider_codex_${seed.runId}`,
      provider_model: "gpt-5.6-codex",
      provider_models_refreshable: false,
      worker_interface_id: seed.worker,
      worker_choice_interface_id: seed.worker,
      door_host_slots: {},
      reasoning_effort: index === 1 ? "high" : "medium",
      created_at_ms: now - Math.round(seed.createdDaysAgo * DAY_MS),
      updated_at_ms: now - Math.round(seed.updatedDaysAgo * DAY_MS),
      next_scheduled_at_ms: seed.nextScheduledAtMs || 0,
      message_count: 0,
      pinned_artifact_ids: [],
      connected_application_ids: index === 1
        ? ["lab_application_project_files"]
        : index === 3
          ? ["lab_application_calendar"]
          : index === 4
            ? ["lab_application_web_search"]
            : [],
    });
  });
  let chatMovementSeq = 1;
  const chatMessages = Object.fromEntries(chats.map((chat, index) => {
    const messages = fixtureChatHistory(chat, index, () => chatMovementSeq++);
    chat.message_count = messages.length;
    chat.last_movement_seq = messages.at(-1)?.movement_seq || 0;
    chat.updated_at_ms = Math.max(chat.updated_at_ms, messages.at(-1)?.created_at_ms || 0);
    const terminal = messages.filter((message) => (
      message.role === "assistant"
      && message.direction === "out"
      && message.event_kind === "message"
      && message.terminal === true
      && message.interface_id === chat.chat_interface_id
      && message.artifact_id
    )).at(-1);
    chat.last_terminal_message = terminal
      ? {
          artifact_id: terminal.artifact_id,
          created_at_ms: terminal.created_at_ms,
          interface_id: terminal.interface_id,
          movement_seq: terminal.movement_seq,
          text: terminal.text.replace(/\s+/g, " ").trim().slice(0, 240),
        }
      : {};
    return [chat.run_id, messages];
  }));

  return {
    projection: fixtureRow({
      generated_at: new Date(now).toISOString(),
      generation: 1,
      mode: "lab",
      repo_root: "lab://octopus-client",
      durable_root: "lab://octopus-client/ledger",
      descriptor_counts: Object.fromEntries(descriptorInventory.map((row) => [row.registry, row.count])),
      ledger_rows: movements.length,
      warnings: [],
    }),
    eeInstances,
    runs,
    runDetails,
    interfaces,
    transferTargets,
    bindings,
    artifacts,
    marks,
    workerEnvironments,
    workerEnvironmentPools,
    workerEnvironmentKinds,
    envelopes,
    pendingArtifacts,
    goals,
    eeTemplates,
    workers,
    tools,
    verifiers,
    policy,
    descriptorInventory,
    runtimeDescriptors,
    descriptorMutations,
    descriptorKinds,
    artifactLabels,
    markKinds,
    projectPresentations,
    applications,
    drivers,
    hostRootRefs,
    chats,
    chatMessages,
    movements,
  };
}

function fixtureChatHistory(chat, index, nextSeq) {
  const rows = [];
  const append = ({
    id,
    role = "assistant",
    label = role === "user" ? "operator.message" : "interface.artifact.output",
    eventKind = "message",
    eventData = {},
    text = "",
    createdAtMs,
    attachments = [],
    error = false,
    terminal = eventKind === "message",
    interfaceId = role === "user" || eventKind === "message" ? chat.chat_interface_id : chat.provider_interface_id,
    latencyMs = 0,
  }) => {
    rows.push(fixtureRow({
      id: `${chat.run_id}:${id}`,
      run_id: chat.run_id,
      role,
      label,
      direction: role === "user" ? "in" : "out",
      event_kind: eventKind,
      event_data: eventData,
      text,
      created_at_ms: createdAtMs,
      movement_seq: nextSeq(),
      artifact_id: `${chat.run_id}:${id}`,
      content_hash: `lab-chat-${chat.run_id}-${id}`,
      interface_id: interfaceId,
      latency_ms: latencyMs,
      attachments,
      error,
      terminal,
    }));
  };

  if (chat.run_id === "lab_run_1") {
    const firstAt = chat.created_at_ms + 45_000;
    append({ id: "overview-request", role: "user", text: "Give me a concise overview of the active projects and what needs attention.", createdAtMs: firstAt });
    append({
      id: "overview-response",
      text: [
        "You have three active areas:",
        "",
        "- **Lumen** — the current product pass is healthy; the connection-status task is ready for review.",
        "- **Research** — the onboarding comparison is complete and pinned for later use.",
        "- **Operations** — the weekly project pulse is scheduled for later today.",
        "",
        "Nothing is blocked. The most useful next step is to review the small Lumen UI change, then decide whether to include it in the next release.",
      ].join("\n"),
      createdAtMs: firstAt + 38_000,
      latencyMs: 38_000,
    });
    const secondAt = chat.updated_at_ms - 48_000;
    append({ id: "today-request", role: "user", text: "What should I focus on next?", createdAtMs: secondAt });
    append({
      id: "today-response",
      text: "Review **Add connection status to project header**. It is a contained change with a passing focused check, so you can inspect the file diff without opening any private project data.",
      createdAtMs: chat.updated_at_ms,
      latencyMs: 48_000,
    });
    return rows;
  }

  if (chat.run_id === "lab_run_2") {
    const turnStart = chat.updated_at_ms - 128_000;
    const turnId = "lab-demo-turn-status-badge";
    const activity = (offset, input) => append({
      ...input,
      createdAtMs: turnStart + offset,
      latencyMs: offset,
      terminal: false,
    });
    append({
      id: "request",
      role: "user",
      text: "Add a compact connection-status badge to the project header. Reuse the existing visual language and run a focused check.",
      createdAtMs: turnStart,
    });
    activity(1_200, { id: "turn-started", eventKind: "turn_started", label: "turn.started", eventData: { lifecycle: "started", turn_id: turnId } });
    activity(7_000, {
      id: "reasoning-inspect",
      eventKind: "reasoning",
      label: "model.reasoning",
      text: "Inspecting the existing project header",
      eventData: { lifecycle: "completed", summary: [{ text: "Inspecting the existing project header" }], turn_id: turnId },
    });
    activity(11_000, {
      id: "list-started",
      eventKind: "dynamic_tool",
      label: "tool.call",
      eventData: { arguments: { path: "src/components" }, call_id: "lab-list-components", status: "started", tool: "coder.list_dir" },
    });
    activity(15_000, {
      id: "list-completed",
      eventKind: "dynamic_tool",
      label: "tool.call",
      eventData: { call_id: "lab-list-components", result: { entries: ["ProjectHeader.tsx", "StatusBadge.tsx"] }, status: "completed", tool: "coder.list_dir" },
    });
    activity(20_000, {
      id: "read-header-started",
      eventKind: "dynamic_tool",
      label: "tool.call",
      eventData: { arguments: { path: "src/components/ProjectHeader.tsx" }, call_id: "lab-read-header", status: "started", tool: "coder.read_file" },
    });
    activity(28_000, {
      id: "read-header-completed",
      eventKind: "dynamic_tool",
      label: "tool.call",
      eventData: { call_id: "lab-read-header", result: { bytes: 1_842 }, status: "completed", tool: "coder.read_file" },
    });
    activity(32_000, {
      id: "read-badge-started",
      eventKind: "dynamic_tool",
      label: "tool.call",
      eventData: { arguments: { path: "src/components/StatusBadge.tsx" }, call_id: "lab-read-badge", status: "started", tool: "coder.read_file" },
    });
    activity(39_000, {
      id: "read-badge-completed",
      eventKind: "dynamic_tool",
      label: "tool.call",
      eventData: { call_id: "lab-read-badge", result: { bytes: 816 }, status: "completed", tool: "coder.read_file" },
    });
    activity(48_000, {
      id: "reasoning-implementation",
      eventKind: "reasoning",
      label: "model.reasoning",
      text: "Reusing the existing status primitive",
      eventData: { lifecycle: "completed", summary: [{ text: "Reusing the existing status primitive" }], turn_id: turnId },
    });
    activity(53_000, {
      id: "search-started",
      eventKind: "dynamic_tool",
      label: "tool.call",
      eventData: { arguments: { path: "src", query: "connectionStatus" }, call_id: "lab-search-status", status: "started", tool: "coder.grep" },
    });
    activity(61_000, {
      id: "search-completed",
      eventKind: "dynamic_tool",
      label: "tool.call",
      eventData: { call_id: "lab-search-status", result: { matches: 3 }, status: "completed", tool: "coder.grep" },
    });
    activity(79_000, {
      id: "file-change",
      eventKind: "file_change",
      label: "file.change",
      text: "Updated ProjectHeader.tsx",
      eventData: {
        changes: [{ added: 12, deleted: 3, kind: "update", path: "src/components/ProjectHeader.tsx" }],
        diff: fixtureProjectHeaderDiff(),
        status: "completed",
      },
    });
    activity(93_000, {
      id: "reasoning-verify",
      eventKind: "reasoning",
      label: "model.reasoning",
      text: "Verifying the focused interaction",
      eventData: { lifecycle: "completed", summary: [{ text: "Verifying the focused interaction" }], turn_id: turnId },
    });
    activity(99_000, {
      id: "check-started",
      eventKind: "dynamic_tool",
      label: "tool.call",
      eventData: { arguments: { command: "npm test -- ProjectHeader" }, call_id: "lab-project-header-check", status: "started", tool: "coder.build" },
    });
    activity(120_000, {
      id: "check-completed",
      eventKind: "dynamic_tool",
      label: "tool.call",
      eventData: {
        call_id: "lab-project-header-check",
        result: { output: "PASS src/components/ProjectHeader.test.tsx\n1 test passed" },
        status: "completed",
        tool: "coder.build",
      },
    });
    activity(126_000, { id: "turn-completed", eventKind: "turn_completed", label: "turn.completed", eventData: { duration_ms: 124_800, lifecycle: "completed", turn_id: turnId } });
    append({
      id: "response",
      text: [
        "Added the connection-status badge to the project header.",
        "",
        "- Reused the existing `StatusBadge` component and semantic tones.",
        "- Kept the project title layout stable at narrow widths.",
        "- Added an accessible status label for screen readers.",
        "- `npm test -- ProjectHeader` passes.",
      ].join("\n"),
      createdAtMs: chat.updated_at_ms,
      latencyMs: 128_000,
    });
    return rows;
  }

  const prompts = [
    "",
    "",
    "Review the empty states in onboarding and identify the smallest useful polish pass.",
    "Prepare a short project pulse for the end of the week.",
    "Compare three strong onboarding patterns and summarize the useful differences.",
    "Make settings navigation feel calmer without changing its information architecture.",
    "Draft release notes from the completed work in this project.",
    "Find why the preview occasionally reconnects after becoming active.",
  ];
  const responses = [
    "",
    "",
    "The empty states now use one action hierarchy and consistent copy. I kept the existing flow intact and recorded the remaining illustration work separately.",
    "The weekly pulse is ready: one completed UI improvement, one research summary, and no blocked work. The scheduled task will refresh it later today.",
    "The strongest pattern is progressive disclosure: show one clear next action, keep optional setup nearby, and preserve a visible way back. I saved a concise comparison for the project.",
    "Settings navigation now keeps section titles stable, uses quieter secondary copy, and preserves keyboard focus across section changes.",
    "Release notes are drafted around outcomes rather than implementation details and are ready for a final wording pass.",
    "The reconnect was caused by a stale preview identity after the tab resumed. The diagnosis is recorded; no private endpoint or machine detail is present in this demo.",
  ];
  const requestAt = chat.updated_at_ms - 58_000;
  append({ id: "request", role: "user", text: prompts[index] || chat.title, createdAtMs: requestAt });
  append({ id: "response", text: responses[index] || "Done.", createdAtMs: chat.updated_at_ms, latencyMs: 58_000 });
  return rows;
}

function fixtureProjectHeaderDiff() {
  return [
    "diff --git a/src/components/ProjectHeader.tsx b/src/components/ProjectHeader.tsx",
    "--- a/src/components/ProjectHeader.tsx",
    "+++ b/src/components/ProjectHeader.tsx",
    "@@ -1,10 +1,19 @@",
    " import { StatusBadge } from \"./StatusBadge\";",
    " ",
    "-export function ProjectHeader({ name }: { name: string }) {",
    "+type Props = { name: string; online: boolean };",
    "+",
    "+export function ProjectHeader({ name, online }: Props) {",
    "   return (",
    "     <header className=\"project-header\">",
    "       <h1>{name}</h1>",
    "+      <StatusBadge",
    "+        label={online ? \"Connected\" : \"Offline\"}",
    "+        tone={online ? \"success\" : \"neutral\"}",
    "+      />",
    "     </header>",
    "   );",
    " }",
  ].join("\n");
}

function fixtureProjectHeaderSourceBefore() {
  return [
    "export function ProjectHeader({ name }: { name: string }) {",
    "  return (",
    "    <header className=\"project-header\">",
    "      <h1>{name}</h1>",
    "    </header>",
    "  );",
    "}",
    "",
  ].join("\n");
}

function fixtureProjectHeaderSource() {
  return [
    "import { StatusBadge } from \"./StatusBadge\";",
    "",
    "type Props = { name: string; online: boolean };",
    "",
    "export function ProjectHeader({ name, online }: Props) {",
    "  return (",
    "    <header className=\"project-header\">",
    "      <h1>{name}</h1>",
    "      <StatusBadge",
    "        label={online ? \"Connected\" : \"Offline\"}",
    "        tone={online ? \"success\" : \"neutral\"}",
    "      />",
    "    </header>",
    "  );",
    "}",
    "",
  ].join("\n");
}

function fixtureStatusBadgeSource() {
  return [
    "type Props = { label: string; tone: \"success\" | \"neutral\" };",
    "",
    "export function StatusBadge({ label, tone }: Props) {",
    "  return <span aria-label={`Project status: ${label}`} data-tone={tone}>{label}</span>;",
    "}",
    "",
  ].join("\n");
}

function fixtureProjectHeaderTestSource() {
  return [
    "import { render, screen } from \"@testing-library/react\";",
    "import { ProjectHeader } from \"./ProjectHeader\";",
    "",
    "it(\"labels the current connection state\", () => {",
    "  render(<ProjectHeader name=\"Lumen\" online />);",
    "  expect(screen.getByLabelText(\"Project status: Connected\")).toBeVisible();",
    "});",
    "",
  ].join("\n");
}

function fixtureInterfaces() {
  const rows = [
    ["lab_system_worker_planner", "System", "bidirectional", "worker"],
    ["lab_system_worker_coder", "System", "bidirectional", "worker"],
    ["lab_system_worker_research", "System", "bidirectional", "worker"],
    ["lab_system_browser", "System", "bidirectional", "service"],
    ["lab_system_exec", "System", "bidirectional", "service"],
    ["lab_world_operator_chat", "World", "inbound", "user"],
    ["lab_world_provider_codex", "World", "bidirectional", "provider"],
    ["lab_world_web_search", "World", "bidirectional", "service"],
    ["lab_world_calendar", "World", "bidirectional", "service"],
    ["lab_world_email", "World", "bidirectional", "service"],
    ["lab_world_files", "World", "bidirectional", "service"],
  ];
  return rows.map(([ruleId, interfaceType, direction, role], index) => fixtureRow({
    id: ruleId,
    rule_id: ruleId,
    interface_type: interfaceType,
    direction,
    role,
    mode: index < 6 ? "active" : "registered",
    tool_ref: role === "service" ? ruleId.replace("lab_", "") : "",
    source: "lab.registry",
    artifact_transfer_protocol_status: "ready",
    artifact_transfer_target_count: 1,
    artifact_transfer_ready_targets: 1,
    artifact_transfer_target_reason: "lab target is ready",
  }));
}

function fixtureEnvironments(at) {
  const rows = [
    [FIXTURE_ROOT_EE_ID, "existence", "active", "", "Coordinate Home, projects, scheduled work, and operator requests"],
    [FIXTURE_PRIMARY_PROJECT_ID, "self_improvement", "active", FIXTURE_ROOT_EE_ID, "Develop and improve the Lumen product"],
    [FIXTURE_RESEARCH_PROJECT_ID, "operator_interaction", "active", FIXTURE_ROOT_EE_ID, "Research product patterns and preserve useful evidence"],
    [FIXTURE_OPERATIONS_PROJECT_ID, "operator_interaction", "idle", FIXTURE_ROOT_EE_ID, "Prepare recurring project updates and operational summaries"],
  ];
  return rows.map(([eeId, template, status, parent, goal], index) => fixtureRow({
    ee_id: eeId,
    template_name: template,
    status,
    goal_id: `lab_goal_${index + 1}`,
    goal,
    parent_ee_id: parent,
    created_at: at(28 - index * 3),
    record_digest: `lab-ee-digest-${index + 1}`,
    allowed_child_ee_templates: index === 0 ? ["self_improvement", "operator_interaction"] : [],
    policy: {
      allowed_interfaces: index === 0
        ? ["lab_system_worker_planner", "lab_world_operator_chat", "lab_world_provider_codex", "lab_world_calendar"]
        : ["lab_system_worker_coder", "lab_system_worker_research", "lab_system_exec", "lab_world_operator_chat", "lab_world_provider_codex", "lab_world_web_search", "lab_world_files"],
      allowed_tools: index === 0
        ? ["artifact_handoff"]
        : ["coder.list_dir", "coder.read_file", "coder.grep", "coder.apply_patch", "coder.build"],
      allowed_worker_environment_kinds: index === 0
        ? ["system_room"]
        : ["git_worktree", "browser_session", "process_session", "web_search"],
    },
  }));
}

function fixtureWorkerEnvironments(environments) {
  const kinds = [
    "system_room",
    "git_worktree",
    "browser_session",
    "process_session",
    "desktop_session",
    "code_mode",
    "remote_exec_server",
    "container_session",
    "mcp_stdio_session",
    "web_search",
    "obsidian_vault",
  ];
  const environmentIds = [
    FIXTURE_ROOT_EE_ID,
    FIXTURE_PRIMARY_PROJECT_ID,
    FIXTURE_PRIMARY_PROJECT_ID,
    FIXTURE_OPERATIONS_PROJECT_ID,
    FIXTURE_RESEARCH_PROJECT_ID,
    FIXTURE_PRIMARY_PROJECT_ID,
    FIXTURE_PRIMARY_PROJECT_ID,
    FIXTURE_RESEARCH_PROJECT_ID,
    FIXTURE_OPERATIONS_PROJECT_ID,
    FIXTURE_RESEARCH_PROJECT_ID,
    FIXTURE_ROOT_EE_ID,
  ];
  return kinds.map((kind, index) => fixtureRow({
    worker_environment_id: `lab_we_${index + 1}`,
    worker_environment_kind: kind,
    kind,
    status: fixtureWorkerEnvironmentStatus(index),
    ee_id: environmentIds[index] || environments[index % environments.length].ee_id,
    display_name: kind === "git_worktree" ? "Lumen worktree" : kind.split("_").join(" "),
    root: kind.includes("worktree") ? FIXTURE_WORKSPACE_ROOT : "",
    session_id: kind.includes("session") ? `lab_session_${index + 1}` : "",
    transport: fixtureWorkerEnvironmentTransport(kind),
    substrate_family: kind.includes("browser")
      ? "browser"
      : kind.includes("container")
        ? "container_runtime"
        : kind.includes("mcp")
          ? "mcp_stdio_room"
          : kind === "code_mode"
            ? "code_mode"
            : "filesystem",
    door: "lab_system_exec",
    descriptor_path: `lab://registry/worker_environment_kind/${kind}.json`,
    capabilities: ["read", index % 2 ? "execute" : "project"],
    tools: index % 2 ? ["shell_exec"] : ["read_projection"],
    client_surfaces: fixtureWorkerEnvironmentSurfaces(kind, index),
  }));
}

function fixtureWorkerEnvironmentSurfaces(kind, index) {
  const surface = (id, surfaceKind, label, data = {}, extra = {}) => fixtureRow({
    id,
    kind: surfaceKind,
    label,
    mode: "fixture",
    status: "available",
    read_only: true,
    data,
    ...extra,
  });
  const terminal = surface(`${kind}_terminal`, "terminal", "Terminal", {
    lines: [
      `$ octopus environment inspect lab_we_${index + 1}`,
      `kind=${kind}`,
      "status=ready",
      "Demo environment ready. No host process was executed.",
    ],
  }, { protocol: "xterm.fixture" });
  const files = surface(`${kind}_files`, "files", "Files", {
    entries: [
      { path: "README.md", kind: "file", text: `# ${kind}\n\nIsolated Worker Environment fixture.\n` },
      { path: "src", kind: "directory" },
      { path: "src/environment.json", kind: "file", text: JSON.stringify({ kind, status: "ready" }, null, 2) },
    ],
  }, { root: `fixture://${kind}` });
  const connection = surface(
    `${kind}_connection`,
    "connection",
    "Connection",
    fixtureWorkerEnvironmentConnection(kind, index),
    { protocol: `connection.${kind}.fixture` },
  );
  if (kind === "browser_session") {
    return [connection, surface("browser_primary", "browser", "Browser", {
      url: "https://example.test/octopus",
      title: "Octopus Test Browser",
      heading: "Browser session",
      body: "A deterministic browser room fixture. Live mode requires a projected viewer endpoint.",
      pages: [
        { url: "https://example.test/octopus", title: "Octopus Test Browser", heading: "Browser session", body: "A deterministic browser room fixture. Live mode requires a projected viewer endpoint." },
        { url: "https://example.test/artifacts", title: "Artifact viewer", heading: "Artifact projection", body: "This page is isolated demo state and has no network authority." },
        { url: "https://example.test/status", title: "Room status", heading: "Browser room ready", body: "Navigation, history, and reload are deterministic inside Test." },
      ],
    }, { protocol: "browser.fixture", viewer_url: "fixture://browser" })];
  }
  if (kind === "desktop_session") {
    return [connection, surface("desktop_primary", "desktop", "Desktop", {
      title: "Desktop session",
      detail: "Isolated remote desktop fixture",
    }, { protocol: "desktop.fixture", viewer_url: "fixture://desktop" })];
  }
  if (kind === "git_worktree") {
    const worktreeFiles = surface("worktree_files", "files", "Files", {
      entries: [
        { path: "README.md", kind: "file", text: "# Lumen\n\nA small demo project used by the isolated Client Lab profile.\n" },
        { path: "package.json", kind: "file", text: JSON.stringify({ name: "lumen-demo", private: true, scripts: { test: "vitest run" } }, null, 2) },
        { path: "src", kind: "directory" },
        { path: "src/components", kind: "directory" },
        { path: "src/components/ProjectHeader.tsx", kind: "file", text: fixtureProjectHeaderSource() },
        { path: "src/components/ProjectHeader.test.tsx", kind: "file", text: fixtureProjectHeaderTestSource() },
        { path: "src/components/StatusBadge.tsx", kind: "file", text: fixtureStatusBadgeSource() },
        { path: "src/styles", kind: "directory" },
        { path: "src/styles/tokens.css", kind: "file", text: ":root {\n  --status-online: #3fb950;\n  --status-offline: #8b949e;\n}\n" },
      ],
    }, { root: `fixture://${FIXTURE_WORKSPACE_ROOT}` });
    return [
      surface("worktree_status", "worktree_status", "Worktree", {
        phase: "worktree-ready",
        label: "lumen",
        description: "Persistent project worktree",
        branch: "feature/project-status",
        worktree_git_root: FIXTURE_WORKSPACE_ROOT,
        worktree_workspace_root: FIXTURE_WORKSPACE_ROOT,
        local_environment_config_path: ".octopus/environment.json",
        worktree_output_text: `Prepared isolated demo worktree at ${FIXTURE_WORKSPACE_ROOT}`,
        setup_output_text: "Resolved project dependencies\nPrepared preview and review surfaces",
      }, { protocol: "worktree.status.fixture" }),
      connection,
      surface("worktree_review", "review", "Review", {
        branch: "feature/project-status",
        base: "main",
        filesChanged: 1,
        additions: 12,
        deletions: 3,
        files: ["src/components/ProjectHeader.tsx"],
        diffs: [
          {
            path: "src/components/ProjectHeader.tsx",
            before: fixtureProjectHeaderSourceBefore(),
            after: fixtureProjectHeaderSource(),
          },
        ],
      }),
      terminal,
      worktreeFiles,
      surface("worktree_preview", "preview", "Preview", {
        url: "http://127.0.0.1:4174",
        title: "Lumen preview",
        detail: "Isolated demo preview; no network request is made.",
      }, { protocol: "browser.fixture", viewer_url: "fixture://preview" }),
    ];
  }
  if (kind === "container_session") {
    return [
      connection,
      terminal,
      files,
      surface("container_runtime", "container", "Runtime", {
        name: "octopus-worker-7f2a",
        image: "ubuntu:24.04 · linux/arm64",
        state: "Running",
        cpu: "18%",
        memory: "612 MB / 4 GB",
        uptime: "36m",
        network: "12.4 MB ↓ · 3.1 MB ↑",
        processes: [
          { pid: "1", command: "octopus-room", cpu: "2.4%", memory: "94 MB" },
          { pid: "28", command: "python worker.py", cpu: "14.8%", memory: "486 MB" },
          { pid: "77", command: "bash", cpu: "0.1%", memory: "7 MB" },
        ],
        ports: [
          { port: "3000/tcp", service: "Preview" },
          { port: "9222/tcp", service: "Browser debugging" },
        ],
        mounts: [
          { target: "/workspace", source: "git_worktree" },
          { target: "/artifacts", source: "artifact cache · read only" },
        ],
      }),
      surface("container_preview", "preview", "Preview", {
        url: "https://preview.example.test/container",
        title: "Container preview",
        detail: "An isolated preview fixture with no network or runtime authority.",
      }, { protocol: "browser.fixture", viewer_url: "fixture://container-preview" }),
    ];
  }
  if (kind === "code_mode") {
    return [connection, surface("code_mode_runtime", "runtime", "Runtime activity", {
      active_cells: ["cell_plan", "cell_patch"],
      pending_tool_calls: [
        { id: "call_17", tool: "artifact.read", status: "pending" },
        { id: "call_18", tool: "exec.run", status: "queued" },
      ],
      detail: "Deterministic Test live-state; no tool process is running.",
    }, { protocol: "worker_environment.live_state.fixture" })];
  }
  if (kind === "mcp_stdio_session") {
    return [connection, surface("mcp_stdio_runtime", "runtime", "MCP server", {
      server: {
        pid: 4217,
        command: "octopus-mcp-stdio-room-tool",
        pending_events: 2,
        protocol_version: "2025-06-18",
      },
      event_log: "initialize → ready\ntools/list → 6 tools\nresources/list → 3 resources",
      stderr_log: "",
    }, { protocol: "mcp_stdio_room.fixture" })];
  }
  if (kind === "web_search") {
    return [connection, surface("web_search_runtime", "runtime", "Search room", {
      detail: "Search results are deterministic Test evidence; no network request is made.",
      server: { provider: "fixture", queries: 3, artifacts: 7 },
    }, { protocol: "web_search.fixture" })];
  }
  if (kind === "obsidian_vault") {
    return [connection, surface("vault_files", "files", "Vault", {
      entries: [
        { path: "README.md", kind: "file", text: "# Test vault\n\nA projected file snapshot for UI development.\n" },
        { path: "Architecture", kind: "directory" },
        { path: "Architecture/Worker Environments.md", kind: "file", text: "# Worker Environments\n\nViews arrive through explicit surface descriptors.\n" },
        { path: "Daily", kind: "directory" },
        { path: "Daily/2026-08-07.md", kind: "file", text: "# 2026-08-07\n\nDemo daily note.\n" },
      ],
    }, { root: "fixture://obsidian-vault" })];
  }
  if (kind === "system_room") return [connection, files];
  return [connection, terminal, files];
}

function fixtureWorkerEnvironmentConnection(kind, index) {
  const suffix = String(index + 1).padStart(2, "0");
  const state = fixtureWorkerEnvironmentStatus(index);
  const common = {
    state,
    sync: state === "released" ? "Final snapshot" : "Current",
    health: state === "released" ? "Released · evidence retained" : state === "idle" ? "Idle · ready to resume" : "Healthy · checked 4s ago",
    capabilities: ["inspect", "attach", "read projected state"],
    events: [
      { state, label: state === "released" ? "Environment released" : "View endpoint ready", detail: "Projected by the Test Worker Environment fixture", time: "now" },
      { state: "active", label: "Environment formed", detail: "Door, kind, and policy assembled for this run", time: "18s" },
      { state: "completed", label: "Initial state synchronized", detail: "Fixture state only; no host files were read", time: "22s" },
    ],
  };
  if (kind === "browser_session") return {
    ...common,
    name: `browser-room-${suffix}`,
    provider: "Chromium browser room",
    address: `cdp+test://browser-room-${suffix}`,
    transport: "CDP viewer",
    workspace: "Ephemeral browser profile",
    persistence: "Run lifetime",
    region: "Local Test boundary",
    capabilities: [...common.capabilities, "navigate", "screenshot"],
  };
  if (kind === "desktop_session") return {
    ...common,
    name: `desktop-room-${suffix}`,
    provider: "Remote desktop room",
    address: `vnc+test://desktop-room-${suffix}`,
    transport: "Desktop viewer",
    workspace: "Isolated desktop session",
    persistence: "Run lifetime",
    region: "Local Test boundary",
    capabilities: [...common.capabilities, "observe desktop", "input preview"],
  };
  if (kind === "git_worktree") return {
    ...common,
    name: "lumen-worktree",
    provider: "Git worktree",
    address: `worktree+fixture://${FIXTURE_WORKSPACE_ROOT}`,
    transport: "Local filesystem room",
    workspace: FIXTURE_WORKSPACE_ROOT,
    persistence: "Persistent branch",
    region: "Local Test boundary",
    capabilities: [...common.capabilities, "review diff", "application preview"],
  };
  if (kind === "remote_exec_server") return {
    ...common,
    name: `remote-worker-${suffix}`,
    provider: "Remote exec room",
    address: `wss://test-worker-${suffix}.invalid/exec`,
    transport: "remote_exec_server",
    workspace: "/home/octopus/workspace",
    persistence: "Reusable connection",
    region: "eu-west · Demo",
    capabilities: [...common.capabilities, "terminal", "file sync"],
  };
  if (kind === "container_session") return {
    ...common,
    name: `octopus-worker-${suffix}`,
    provider: "Local container",
    address: `docker+test://octopus-worker-${suffix}`,
    transport: "Container process room",
    workspace: "/workspace",
    persistence: "Persistent filesystem",
    region: "Local Test boundary",
    capabilities: [...common.capabilities, "terminal", "files", "runtime metrics"],
  };
  return {
    ...common,
    name: `${kind.replaceAll("_", "-")}-${suffix}`,
    provider: kind === "process_session" ? "Local process room" : "Execution room",
    address: `room+test://${kind}/${suffix}`,
    transport: kind === "process_session" ? "Local process" : "Projected room",
    workspace: "/workspace",
    persistence: kind === "process_session" ? "Run lifetime" : "Descriptor-defined",
    region: "Local Test boundary",
  };
}

function fixtureRuns(at, environments, workerEnvironments) {
  const workers = [
    "lab_system_worker_planner",
    "lab_system_worker_coder",
    "lab_system_worker_coder",
    "lab_system_worker_planner",
    "lab_system_worker_research",
    "lab_system_worker_coder",
  ];
  return Array.from({ length: workerEnvironments.length }, (_, index) => {
    const workerEnvironment = workerEnvironments[index];
    const environment = environments.find((candidate) => candidate.ee_id === workerEnvironment.ee_id)
      ?? environments[index % environments.length];
    const workerEnvironmentStatus = fixtureWorkerEnvironmentStatus(index);
    return fixtureRow({
      run_id: `lab_run_${index + 1}`,
      status: workerEnvironmentStatus === "released" ? "completed" : workerEnvironmentStatus,
      worker: workers[index % workers.length],
      ee_id: environment.ee_id,
      goal: environment.goal,
      created_at: at(Math.max(0, 28 - index * 2), index),
      worker_environment_id: workerEnvironment.worker_environment_id,
      source: "lab.run_record",
      meta: { record_path: `lab://runs/lab_run_${index + 1}.json` },
    });
  });
}

function fixtureWorkerEnvironmentStatus(index) {
  return ["active", "released", "active", "idle", "released", "active"][index % 6];
}

function fixtureWorkerEnvironmentTransport(kind) {
  if (kind === "browser_session") return "browser.fixture";
  if (kind === "desktop_session") return "desktop.fixture";
  if (kind === "remote_exec_server") return "xterm.websocket.fixture";
  if (kind === "container_session") return "container_runtime.fixture";
  if (kind === "mcp_stdio_session") return "mcp_stdio_room.fixture";
  if (kind === "code_mode") return "code_mode.live_state.fixture";
  if (kind === "process_session") return "terminal.fixture";
  return "projection.fixture";
}

function fixtureCheckpoints(run, runIndex, at) {
  return Array.from({ length: runIndex % 2 ? 2 : 3 }, (_, index) => {
    const projected = index === 2 && run.status === "active";
    return fixtureRow({
      checkpoint_id: `${run.run_id}:checkpoint:${index + 1}`,
      label: ["Receive context", "Execute task", "Verify output"][index],
      status: projected ? "projected" : "made",
      interface: index === 0 ? "lab_world_operator_chat" : "lab_system_worker_planner",
      out_interfaces: index === 2 ? ["lab_world_operator_chat"] : ["lab_world_provider_codex"],
      tool_name: index === 1 ? "shell_exec" : "artifact_handoff",
      artifact_label: ["context.reference", "llm.request", "interface.episode.output"][index],
      artifact_bindings: index + 1,
      interface_bindings: 2,
      recorded_at_unix_ms: Date.parse(at(Math.max(0, 20 - runIndex * 2 - index))),
      required_verified_artifacts: index === 2 ? [{ mark: "llm.output.ok", artifact_label: "interface.episode.output", verifier: "artifact_schema", outcome: projected ? "declared" : "closed" }] : [],
    });
  });
}

function fixtureApplicationProject(environment) {
  return fixtureRow({
    ee_id: environment.ee_id,
    goal: fixtureEnvironmentDisplayName(environment.ee_id),
    parent_ee_id: environment.parent_ee_id,
    record_digest: environment.record_digest,
  });
}

function fixtureEnvironmentDisplayName(eeId) {
  if (eeId === FIXTURE_ROOT_EE_ID) return "Home";
  if (eeId === FIXTURE_PRIMARY_PROJECT_ID) return "Lumen";
  if (eeId === FIXTURE_RESEARCH_PROJECT_ID) return "Research";
  if (eeId === FIXTURE_OPERATIONS_PROJECT_ID) return "Operations";
  return eeId;
}

function fixtureApplications(environments) {
  const definitions = [
    {
      id: "lab_application_discord",
      name: "Discord",
      description: "Use Discord messages in the tasks you choose.",
      category: "connector",
      icon: "message",
      logoDataUrl: "./lab/application-icons/discord.png",
      interfaces: ["lab_world_discord"],
      available: [FIXTURE_PRIMARY_PROJECT_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_email",
      name: "Email",
      description: "Read and send email from the tasks you choose.",
      category: "connector",
      icon: "email",
      logoDataUrl: "./lab/application-icons/email.png",
      interfaces: ["lab_world_email"],
      available: [FIXTURE_OPERATIONS_PROJECT_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_matrix",
      name: "Matrix",
      description: "Use Matrix rooms and messages in the tasks you choose.",
      category: "connector",
      icon: "message",
      logoDataUrl: "./lab/application-icons/matrix.png",
      interfaces: ["lab_world_matrix"],
      available: [FIXTURE_RESEARCH_PROJECT_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_slack",
      name: "Slack",
      description: "Use Slack conversations in the tasks where it is enabled.",
      category: "connector",
      icon: "message",
      logoDataUrl: "./lab/application-icons/slack.png",
      interfaces: ["lab_world_slack"],
      available: [FIXTURE_PRIMARY_PROJECT_ID, FIXTURE_OPERATIONS_PROJECT_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_speaker",
      name: "Speaker",
      description: "Play spoken responses through your Mac's audio output.",
      category: "connector",
      icon: "voice",
      logoDataUrl: "./lab/application-icons/speaker.png",
      interfaces: ["lab_world_speaker"],
      available: [FIXTURE_ROOT_EE_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_speech_to_text",
      name: "Speech to Text",
      description: "Turn speech recordings into text.",
      category: "connector",
      icon: "voice",
      logoDataUrl: "./lab/application-icons/speech-to-text.png",
      interfaces: ["lab_world_speech_to_text"],
      available: [FIXTURE_ROOT_EE_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_telegram",
      name: "Telegram",
      description: "Use Telegram chats in the tasks you choose.",
      category: "connector",
      icon: "message",
      logoDataUrl: "./lab/application-icons/telegram.png",
      interfaces: ["lab_world_telegram"],
      available: [FIXTURE_OPERATIONS_PROJECT_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_text_to_speech",
      name: "Text to Speech",
      description: "Turn written responses into natural speech.",
      category: "connector",
      icon: "voice",
      logoDataUrl: "./lab/application-icons/text-to-speech.png",
      interfaces: ["lab_world_text_to_speech"],
      available: [FIXTURE_ROOT_EE_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_whatsapp",
      name: "WhatsApp",
      description: "Use WhatsApp conversations in the tasks you choose.",
      category: "connector",
      icon: "message",
      logoDataUrl: "./lab/application-icons/whatsapp.png",
      interfaces: ["lab_world_whatsapp"],
      available: [FIXTURE_ROOT_EE_ID, FIXTURE_OPERATIONS_PROJECT_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_google_calendar",
      name: "Google Calendar",
      description: "Work with Google Calendar events and schedules.",
      category: "plugin",
      icon: "calendar",
      logoDataUrl: "./lab/application-icons/google-calendar.png",
      interfaces: ["lab_world_mcp_google_calendar"],
      available: [FIXTURE_ROOT_EE_ID, FIXTURE_OPERATIONS_PROJECT_ID],
      selected: [FIXTURE_OPERATIONS_PROJECT_ID],
      connected: ["lab_run_4"],
    },
    {
      id: "lab_application_google_drive",
      name: "Google Drive",
      description: "Find and work with files in Google Drive.",
      category: "plugin",
      icon: "files",
      logoDataUrl: "./lab/application-icons/google-drive.png",
      interfaces: ["lab_world_mcp_google_drive"],
      available: [FIXTURE_PRIMARY_PROJECT_ID],
      selected: [FIXTURE_PRIMARY_PROJECT_ID],
      connected: ["lab_run_2"],
    },
    {
      id: "lab_application_image_generation",
      name: "Image generation",
      description: "Create images in tasks where visual work is enabled.",
      category: "plugin",
      icon: "image",
      logoDataUrl: "./lab/application-icons/image-generation.png",
      interfaces: ["lab_world_image_generation"],
      available: [FIXTURE_PRIMARY_PROJECT_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_outlook_email",
      name: "Outlook Email",
      description: "Read, search, and work with Outlook email.",
      category: "plugin",
      icon: "email",
      logoDataUrl: "./lab/application-icons/outlook-email.png",
      interfaces: ["lab_world_mcp_microsoft_outlook_email"],
      available: [FIXTURE_OPERATIONS_PROJECT_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_notion",
      name: "Notion",
      description: "Find and work with pages in Notion.",
      category: "plugin",
      icon: "files",
      logoDataUrl: "./lab/application-icons/notion.png",
      interfaces: ["lab_world_mcp_notion"],
      available: [FIXTURE_PRIMARY_PROJECT_ID, FIXTURE_RESEARCH_PROJECT_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_slack_tools",
      name: "Slack Tools",
      description: "Search and work with content in Slack.",
      category: "plugin",
      icon: "plugins",
      logoDataUrl: "./lab/application-icons/slack-tools.png",
      interfaces: ["lab_world_mcp_slack"],
      available: [FIXTURE_PRIMARY_PROJECT_ID, FIXTURE_OPERATIONS_PROJECT_ID],
      selected: [],
      connected: [],
    },
    {
      id: "lab_application_project_files",
      name: "Project files",
      description: "Attach a selected project artifact as task context.",
      category: "artifact",
      icon: "files",
      interfaces: [],
      artifactBindings: [{ artifact_id: "lab_artifact_project_brief", label: "project.brief" }],
      available: [FIXTURE_PRIMARY_PROJECT_ID],
      selected: [FIXTURE_PRIMARY_PROJECT_ID],
      connected: ["lab_run_2"],
    },
  ];
  return definitions.map((definition, index) => fixtureRow({
    id: definition.id,
    name: definition.name,
    description: definition.description,
    category: definition.category,
    developer: "Octopus",
    version: "1.0.0",
    icon: definition.icon,
    logo: "",
    logo_dark: "",
    logo_data_url: definition.logoDataUrl ?? "",
    logo_dark_data_url: definition.logoDataUrl ?? "",
    interfaces: definition.interfaces,
    artifact_bindings: definition.artifactBindings ?? [],
    installed: true,
    valid: true,
    missing_interface_ids: [],
    descriptor_path: `lab://applications/${definition.id}/application.json`,
    manifest_path: `lab://applications/${definition.id}/application.json`,
    manifest_digest: `lab-application-digest-${index + 1}`,
    connected_run_ids: definition.connected,
    project_states: environments.map((environment) => fixtureRow({
      ee_id: environment.ee_id,
      is_root: environment.ee_id === FIXTURE_ROOT_EE_ID,
      policy_allowed: definition.available.includes(environment.ee_id),
      ceiling_allowed: true,
      selected_by_default: definition.selected.includes(environment.ee_id),
      record_digest: environment.record_digest,
    })),
  }));
}

function fixtureDrivers(environments, workerEnvironments) {
  const definitions = [
    {
      name: "git_worktree",
      substrate: "filesystem",
      capabilities: ["files", "git", "process exec", "review"],
      executors: ["exec"],
      tools: ["coder.list_dir", "coder.read_file", "coder.grep", "coder.apply_patch", "coder.build"],
      doors: ["lab_system_exec"],
      available: [FIXTURE_PRIMARY_PROJECT_ID],
      selected: [FIXTURE_PRIMARY_PROJECT_ID],
      hostSlots: { workspace_root: FIXTURE_WORKSPACE_ROOT },
      network: "restricted",
      sourceFileCount: 28,
    },
    {
      name: "browser_session",
      substrate: "browser",
      capabilities: ["navigate", "inspect", "screenshot"],
      executors: ["browser"],
      tools: ["browser.navigate"],
      doors: ["lab_system_browser"],
      available: [FIXTURE_PRIMARY_PROJECT_ID, FIXTURE_RESEARCH_PROJECT_ID],
      selected: [FIXTURE_RESEARCH_PROJECT_ID],
      hostSlots: { browser_profile_root: "/workspace/.browser-profile" },
      network: "restricted",
      sourceFileCount: 19,
    },
    {
      name: "process_session",
      substrate: "process",
      capabilities: ["process exec", "terminal", "files"],
      executors: ["exec"],
      tools: ["coder.build"],
      doors: ["lab_system_exec"],
      available: [FIXTURE_PRIMARY_PROJECT_ID, FIXTURE_OPERATIONS_PROJECT_ID],
      selected: [],
      hostSlots: { workspace_root: FIXTURE_WORKSPACE_ROOT },
      network: "denied",
      sourceFileCount: 16,
    },
  ];
  return definitions.map((definition, index) => {
    const activeInstances = workerEnvironments.filter((environment) => (
      environment.worker_environment_kind === definition.name
      && environment.status !== "released"
    ));
    const projectStates = environments.map((environment) => fixtureRow({
      ee_id: environment.ee_id,
      is_root: environment.ee_id === FIXTURE_ROOT_EE_ID,
      policy_allowed: definition.available.includes(environment.ee_id),
      ceiling_allowed: true,
      selected_by_default: definition.selected.includes(environment.ee_id),
      record_digest: environment.record_digest,
      host_slots: definition.available.includes(environment.ee_id) ? definition.hostSlots : {},
    }));
    const projectConfigured = projectStates.some((state) => state.policy_allowed || state.selected_by_default);
    const packageDigest = `sha256:${String(index + 1).repeat(64).slice(0, 64)}`;
    return fixtureRow({
      name: definition.name,
      package_id: `driver.${definition.name}`,
      installed: true,
      source_available: true,
      source_digest: packageDigest,
      installed_digest: packageDigest,
      source_file_count: definition.sourceFileCount,
      receipt_state: "valid",
      install_block_reason: "Driver is already installed.",
      removal_allowed: activeInstances.length === 0 && !projectConfigured,
      removal_block_reason: activeInstances.length > 0
        ? `Release ${activeInstances.length} active worker environment${activeInstances.length === 1 ? "" : "s"} before removing this Driver.`
        : projectConfigured
          ? "Remove this Driver from every project before removing its package."
          : "",
      substrate_family: definition.substrate,
      worker_consent: "explicit_ceiling",
      capabilities: definition.capabilities,
      supported_executor_kinds: definition.executors,
      instance_config_files: [],
      door_interfaces: definition.doors,
      door_driver_refs: [`lab.${definition.name}`],
      tool_ids: definition.tools,
      tool_bindings: definition.tools.map((toolId) => fixtureRow({
        tool_id: toolId,
        interface_id: definition.doors[0],
      })),
      boundary_defaults: {
        read_root_refs: Object.entries(definition.hostSlots).map(([path, root]) => ({ access: ["read"], path, root })),
        write_root_refs: Object.entries(definition.hostSlots).map(([path, root]) => ({ access: ["write"], path, root })),
        protected_read_only_subpaths: [],
        network_ceiling: definition.network,
      },
      lifecycle: { requires_release: definition.name !== "git_worktree" },
      descriptor_path: `lab://drivers/${definition.name}/worker_environment_kind.json`,
      active_instances: activeInstances,
      project_states: projectStates,
    });
  });
}

function fixtureTaskAuthoringProfiles(environments) {
  return environments.map((environment) => {
    const worker = fixtureEnvironmentWorker(environment.ee_id);
    return fixtureRow({
      ee_id: environment.ee_id,
      worker_choice_interface: worker,
      selected_worker_environment_kinds: fixtureSelectedDriverNames(environment.ee_id),
    });
  });
}

function fixtureChatExecutionContexts(data) {
  return data.eeInstances.map((environment) => {
    const workerId = fixtureEnvironmentWorker(environment.ee_id);
    const applicationChoices = data.applications.flatMap((application) => {
      const state = application.project_states.find((candidate) => candidate.ee_id === environment.ee_id);
      return state?.policy_allowed ? [{ id: application.id, selected_by_default: state.selected_by_default }] : [];
    });
    const driverChoices = data.drivers.flatMap((driver) => {
      const state = driver.project_states.find((candidate) => candidate.ee_id === environment.ee_id);
      if (!state?.policy_allowed) return [];
      return [{
        name: driver.name,
        capabilities: driver.capabilities,
        root_slots: driver.name === "filesystem_workspace"
          ? ["host_filesystem_root", "workspace_root"]
          : [],
        tool_ids: driver.tool_ids,
        door_interfaces: driver.door_interfaces,
        host_slots: state.host_slots,
        selected_by_default: state.selected_by_default,
      }];
    });
    const workspaceSlots = environment.ee_id === FIXTURE_PRIMARY_PROJECT_ID
      ? { lab_system_exec: { workspace_root: FIXTURE_WORKSPACE_ROOT } }
      : {};
    const agent = environment.ee_id === FIXTURE_ROOT_EE_ID ? null : {
      id: `lab_agent_${environment.ee_id}`,
      name: environment.ee_id === FIXTURE_PRIMARY_PROJECT_ID ? "Octopus Development" : "Research Development",
      icon: environment.ee_id === FIXTURE_PRIMARY_PROJECT_ID ? "glasses" : "curious",
    };
    return fixtureRow({
      ee_id: environment.ee_id,
      agent_catalog_digest: "lab-agent-catalog",
      default_task_access_mode: "project_access",
      host_root_refs: data.hostRootRefs,
      is_os_root: environment.ee_id === FIXTURE_ROOT_EE_ID,
      display_name: data.projectPresentations.find((presentation) => presentation.ee_id === environment.ee_id)?.name
        || (environment.ee_id === FIXTURE_ROOT_EE_ID ? "Home" : environment.goal),
      goal: environment.goal,
      template_name: environment.template_name,
      parent_ee_id: environment.parent_ee_id,
      workers: [{
        id: workerId,
        label: workerId === "lab_system_worker_coder" ? "Coder" : workerId === "lab_system_worker_research" ? "Research" : "Operator",
        authoring_profile: true,
        agent_source_template: agent !== null,
        client_agent_id: agent?.id ?? "",
        client_agent_name: agent?.name ?? "",
        client_agent_icon: agent?.icon ?? "",
        client_agent_created_at_ms: agent ? 1 : 0,
        client_agent_updated_at_ms: agent ? 1 : 0,
        client_agent_deleted_at_ms: 0,
        client_template: environment.ee_id !== FIXTURE_ROOT_EE_ID,
        client_template_provider_interface_id: "lab_world_provider_codex",
        client_template_reasoning_effort: "high",
        client_template_digest: "lab-client-template",
        client_template_application_ids: applicationChoices.map((application) => application.id),
        client_template_worker_environment_kinds: driverChoices.map((driver) => driver.name),
        provider_interface_ids: ["lab_world_provider_codex"],
        role_artifact_ids: [],
        authoring_room_interface_id: "lab_system_exec",
        door_host_slots: workspaceSlots,
        external_inputs: [{ checkpoint_id: "cp_operator_message", interface_id: "lab_world_operator_chat" }],
        external_relays: [],
        model_outputs: [{
          checkpoint_id: "cp_provider_output",
          interface_id: "lab_world_provider_codex",
          required_verified_artifact: {
            mark: "llm.output.ok",
            artifact_label: "interface.artifact.output",
            verifier: "artifact_schema",
            params: {},
          },
        }],
        context_artifact_bindings: [],
        selected_worker_environment_kinds: fixtureSelectedDriverNames(environment.ee_id),
        applications: applicationChoices,
        drivers: driverChoices,
      }],
    });
  });
}

function fixtureEnvironmentWorker(eeId) {
  if (eeId === FIXTURE_PRIMARY_PROJECT_ID) return "lab_system_worker_coder";
  if (eeId === FIXTURE_RESEARCH_PROJECT_ID) return "lab_system_worker_research";
  return "lab_system_worker_planner";
}

function fixtureSelectedDriverNames(eeId) {
  if (eeId === FIXTURE_PRIMARY_PROJECT_ID) return ["git_worktree"];
  if (eeId === FIXTURE_RESEARCH_PROJECT_ID) return ["browser_session"];
  return [];
}

function fixtureProviderStatus(chats) {
  const home = chats.find((chat) => chat.client_surface_role === "home");
  return [{
    provider_interface_id: "lab_world_provider_codex",
    observed_at_ms: Date.now(),
    movement_seq: home?.last_movement_seq || 0,
    run_id: home?.run_id || "",
    rate_limits: {
      credits: null,
      individualLimit: null,
      limitId: "lab-demo",
      limitName: "Demo allowance",
      planType: "demo",
      primary: { resetsAt: null, usedPercent: 18, windowDurationMins: 300 },
      secondary: null,
    },
  }];
}

function fixtureMovements(now, runs, interfaces, artifacts) {
  const weights = Array.from({ length: 30 }, (_, day) => 1.2 + 5.2 * Math.exp(-((day - 15) ** 2) / 28) + 2.1 * Math.exp(-((day - 21) ** 2) / 9));
  const counts = distribute(243, weights);
  const rows = [];
  let index = 0;
  for (let day = 0; day < counts.length; day += 1) {
    for (let occurrence = 0; occurrence < counts[day]; occurrence += 1) {
      const run = runs[index % runs.length];
      const artifact = artifacts[index % artifacts.length];
      const boundary = interfaces[(index + 4) % interfaces.length];
      const inbound = index % 2 === 0;
      const timestamp = now - day * DAY_MS - ((occurrence * 71 + index * 17) % 1_200) * 60_000;
      const verifierPassed = index % 11 === 0;
      const verifierFailed = !verifierPassed && index % 29 === 0;
      const mark = ["coder.build.ok", "artifact.admitted", "run.checkpoint.closed"][index % 3];
      rows.push(fixtureRow({
        movement_id: `lab_movement_${String(index + 1).padStart(3, "0")}`,
        seq: 243 - index,
        lamport: 2_000 + index,
        trace: `lab_trace_${Math.floor(index / 3) + 1}`,
        kind: verifierPassed ? `MarkOutcomeAdmitted(${mark})` : verifierFailed ? "VerificationFailed" : inbound ? "InterfaceArrival" : "InterfaceOutbound",
        mark,
        verifier: mark === "coder.build.ok" ? "build_success" : mark === "artifact.admitted" ? "artifact_schema" : "mark_admission",
        recorded_at_unix_ms: timestamp,
        artifact_id: artifact.artifact_id,
        artifact_label: artifact.label,
        content_hash: `fixture${String(index).padStart(58, "0")}`.slice(0, 64),
        blob_path: "",
        blob_size: artifact.size,
        from: inbound
          ? { kind: "interface", id: boundary.rule_id }
          : { kind: "run", id: run.run_id, ee_id: run.ee_id },
        to: inbound
          ? { kind: "run", id: run.run_id, ee_id: run.ee_id }
          : { kind: "interface", id: boundary.rule_id },
        raw_run_id: run.run_id,
        raw_ee_id: run.ee_id,
        raw_worker: run.worker,
      }));
      index += 1;
    }
  }
  return rows.sort((left, right) => right.recorded_at_unix_ms - left.recorded_at_unix_ms);
}

function fixtureProviderInterfaceDescriptors() {
  return [
    {
      id: "lab_world_provider_codex",
      label: "Codex",
      descriptorPath: "lab://registry/interface/lab_world_provider_codex/interface.json",
      runtimeHandlerRef: "client.lab_simulation",
      sourceNames: ["codex"],
      model: "gpt-5.6-codex",
      reasoningEffort: "high",
      supportedReasoningEfforts: ["low", "medium", "high", "extra_high"],
      models: [{
        model: "gpt-5.6-codex",
        displayName: "GPT-5.6 Codex",
        description: "Test provider model",
        defaultReasoningEffort: "high",
        supportedReasoningEfforts: ["low", "medium", "high", "extra_high"],
        showInPicker: true,
        isDefault: true,
      }],
      preferred: true,
    },
  ];
}

function distribute(total, weights) {
  const sum = weights.reduce((value, weight) => value + weight, 0);
  const exact = weights.map((weight) => (weight / sum) * total);
  const values = exact.map(Math.floor);
  let remaining = total - values.reduce((value, count) => value + count, 0);
  const order = exact.map((value, index) => ({ index, remainder: value - Math.floor(value) })).sort((left, right) => right.remainder - left.remainder);
  for (let index = 0; index < remaining; index += 1) values[order[index].index] += 1;
  return values;
}

function filterTargets(rows, params) {
  return filterString(filterString(filterString(rows, "interface_id", params.interface_id ?? params.interface), "run_id", params.run_id), "status", params.status);
}

function filterString(rows, field, value) {
  const needle = typeof value === "string" ? value.trim().toLowerCase() : "";
  return needle ? rows.filter((row) => String(row[field] || "").toLowerCase() === needle) : rows;
}

function fixtureMovementPage(rows, params) {
  const beforeSeq = optionalSequence(params.before_seq);
  const afterSeq = optionalSequence(params.after_seq);
  const afterTime = optionalSequence(params.recorded_after_unix_ms);
  const beforeTime = optionalSequence(params.recorded_before_unix_ms);
  const query = typeof params.query === "string" ? params.query.trim().toLowerCase() : "";
  const limit = Math.min(1_000, Math.max(1, Number.isFinite(Number(params.limit)) ? Math.floor(Number(params.limit)) : 500));
  const matching = rows.filter((row) => {
    const seq = Number(row.seq);
    const timestamp = Number(row.recorded_at_unix_ms);
    if (beforeSeq !== null && seq >= beforeSeq) return false;
    if (afterSeq !== null && seq <= afterSeq) return false;
    if (afterTime !== null && timestamp < afterTime) return false;
    if (beforeTime !== null && timestamp > beforeTime) return false;
    if (!query) return true;
    return [row.kind, row.trace, row.artifact_id, row.artifact_label, row.from?.id, row.to?.id]
      .some((value) => String(value || "").toLowerCase().includes(query));
  }).sort((left, right) => Number(right.seq) - Number(left.seq));
  const movements = matching.slice(0, limit);
  const oldest = movements.length ? Math.min(...movements.map((row) => Number(row.seq))) : null;
  return {
    movements,
    total: matching.length,
    ...(oldest !== null && movements.length < matching.length ? { next_before_seq: oldest } : {}),
  };
}

function fixtureRunActivityPage(runId, rows, params) {
  const afterSeq = optionalSequence(params.after_seq);
  const beforeSeq = optionalSequence(params.before_seq);
  const fromEnd = params.from_end === true;
  const limit = Math.min(1_000, Math.max(1, Number.isFinite(Number(params.limit)) ? Math.floor(Number(params.limit)) : 100));
  if (afterSeq !== null && beforeSeq !== null && beforeSeq <= afterSeq) {
    throw new Error("run activity before_seq must be greater than after_seq");
  }
  const ordered = rows.slice().sort((left, right) => Number(left.movement_seq) - Number(right.movement_seq));
  const matching = ordered.filter((row) => {
    const seq = Number(row.movement_seq);
    return (afterSeq === null || seq > afterSeq) && (beforeSeq === null || seq < beforeSeq);
  });
  const selected = (fromEnd || beforeSeq !== null ? matching.slice(-limit) : matching.slice(0, limit)).map((message) => ({
    ...message,
    payload_error: "",
  }));
  const oldestSeq = selected.length ? Number(selected[0].movement_seq) : null;
  const latestSeq = selected.length ? Number(selected[selected.length - 1].movement_seq) : null;
  const allOldest = ordered.length ? Number(ordered[0].movement_seq) : null;
  const allLatest = ordered.length ? Number(ordered[ordered.length - 1].movement_seq) : null;
  return {
    run_id: runId,
    total: matching.length,
    truncated: selected.length < matching.length,
    has_older: oldestSeq !== null && allOldest !== null && allOldest < oldestSeq,
    has_newer: latestSeq !== null && allLatest !== null && allLatest > latestSeq,
    oldest_seq: oldestSeq,
    latest_seq: latestSeq,
    activity: selected,
  };
}

function optionalSequence(value) {
  if (value === undefined || value === null || value === "") return null;
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < 0) throw new Error("movement cursor and timestamp parameters must be unsigned integers");
  return number;
}

function limitRows(rows, params) {
  const limit = Number(params.limit);
  return Number.isFinite(limit) && limit >= 0 ? rows.slice(0, Math.floor(limit)) : rows.slice();
}

function fixtureRow(value) {
  return { ...value, projection_source: "lab" };
}

function requiredString(value, field) {
  const text = typeof value === "string" ? value.trim() : "";
  if (!text) throw new Error(`${field} is required`);
  return text;
}

module.exports = {
  createFixtureProjectionStore,
  fixtureProviderInterfaceDescriptors,
};
