module.exports = { workerEnvironmentsProjection };

const fs = require("node:fs/promises");
const path = require("node:path");
const { readJsonFile } = require("./jsonFiles.cjs");
const { projectedTextFile } = require("./movementRows.cjs");
const { listWorkerEnvironmentRecordFiles } = require("./providerDescriptors.cjs");
const { projectedFilesystemRootSurfaces, projectedGitSurfaces, projectedWorktreeStatusSurfaces, unavailableViewerSurface, validAddressWithProtocols, validHttpAddress, validWebSocketAddress, validWorkerEnvironmentLiveState, workerEnvironmentCapabilities, workerEnvironmentLiveAvailable, workerEnvironmentSession, workerEnvironmentWorkspace } = require("./registryRows.cjs");
const { arrayValue, isPathInside, numberValue, objectOrEmpty, safeInterfacePathSegment, stringValue, validWorkerEnvironmentFormation } = require("./values.cjs");

function workerEnvironmentsProjection(store) {
  async function readWorkerEnvironmentRecords(octopusHomeRoot, warnings = [], kindDescriptors = new Map()) {
    const interfacesRoot = path.join(octopusHomeRoot, "interfaces");
    const files = await listWorkerEnvironmentRecordFiles(interfacesRoot);
    const rows = [];
    for (const file of files) {
      const relativeParts = path.relative(interfacesRoot, file).split(path.sep);
      if (relativeParts.length !== 4
        || !safeInterfacePathSegment(relativeParts[0])
        || relativeParts[1] !== "worker-environments"
        || relativeParts[3] !== "worker_environment.json") {
        warnings.push(`worker environment record is outside the canonical interface store layout: ${file}`);
        continue;
      }
      let record;
      try {
        record = await readJsonFile(file);
      } catch (error) {
        warnings.push(String(error?.message ?? error));
        continue;
      }
      const formation = objectOrEmpty(record?.formation);
      const workerEnvironmentId = stringValue(formation.worker_environment_id);
      const workerEnvironmentKind = stringValue(formation.worker_environment_kind);
      const lifecycle = stringValue(record?.lifecycle).toLowerCase();
      const validFormation = record?.schema_version === 5
        && validWorkerEnvironmentFormation(formation)
        && workerEnvironmentId
        && workerEnvironmentId === path.basename(path.dirname(file))
        && workerEnvironmentKind
        && stringValue(formation.run)
        && stringValue(formation.ee)
        && stringValue(formation.worker)
        && stringValue(formation.door_interface)
        && stringValue(formation.door_interface) === relativeParts[0]
        && ["active", "released", "failed"].includes(lifecycle)
        && Number.isSafeInteger(record.opened_at_movement_seq)
        && Number.isSafeInteger(record.last_activity_movement_seq)
        && record.last_activity_movement_seq >= record.opened_at_movement_seq;
      if (!validFormation) {
        warnings.push(`worker environment record does not satisfy formation schema v5: ${file}`);
        continue;
      }

      const interfaceVarDir = path.join(interfacesRoot, relativeParts[0]);
      const liveStatePath = path.join(path.dirname(file), "live_state.json");
      let liveState = null;
      try {
        const liveStateStat = await fs.lstat(liveStatePath).catch((error) => {
          if (error?.code === "ENOENT") return null;
          throw error;
        });
        if (liveStateStat?.isSymbolicLink()) {
          warnings.push(`worker environment live state must not be a symbolic link: ${liveStatePath}`);
        } else if (liveStateStat?.isFile()) {
          liveState = await readJsonFile(liveStatePath);
        } else if (liveStateStat) {
          warnings.push(`worker environment live state is not a regular file: ${liveStatePath}`);
        }
      } catch (error) {
        warnings.push(String(error?.message ?? error));
      }
      if (liveState !== null && !validWorkerEnvironmentLiveState(
        liveState,
        workerEnvironmentId,
        workerEnvironmentKind,
      )) {
        warnings.push(`worker environment live state does not match its formation: ${liveStatePath}`);
        liveState = null;
      }

      const live = objectOrEmpty(liveState);
      const rootBindings = Object.entries(objectOrEmpty(formation.door_root_bindings));
      const viewer = objectOrEmpty(live.viewer);
      const liveStatus = stringValue(live.status).toLowerCase();
      const liveAvailable = workerEnvironmentLiveAvailable(lifecycle, liveStatus);
      const kindDescriptor = kindDescriptors.get(workerEnvironmentKind);
      const kindCapabilities = arrayValue(kindDescriptor?.capabilities).map(stringValue).filter(Boolean);
      rows.push({
        worker_environment_id: workerEnvironmentId,
        formation_pin: stringValue(formation.formation_pin),
        worker_environment_kind: workerEnvironmentKind,
        kind: workerEnvironmentKind,
        status: lifecycle === "active" ? liveStatus || "not projected" : lifecycle,
        live_available: liveAvailable,
        ee_id: stringValue(formation.ee),
        run_id: stringValue(formation.run),
        worker: stringValue(formation.worker),
        door: stringValue(formation.door_interface),
        substrate_family: stringValue(live.substrate_family),
        root: workerEnvironmentWorkspace(live, rootBindings),
        session_id: workerEnvironmentSession(live),
        transport: stringValue(viewer.protocol),
        capabilities: workerEnvironmentCapabilities(formation, kindCapabilities),
        tools: arrayValue(formation.granted_tools),
        tool_descriptor_hashes: objectOrEmpty(formation.tool_descriptor_hashes),
        client_surfaces: await projectedWorkerEnvironmentSurfaces({
          octopusHomeRoot,
          formation,
          interfaceVarDir,
          lifecycle,
          liveState: live,
          kindCapabilities,
          openedAtMovementSeq: record.opened_at_movement_seq,
          lastActivityMovementSeq: record.last_activity_movement_seq,
          warnings,
        }),
        description: stringValue(live.detail),
        record_path: file,
        live_state_path: liveState ? liveStatePath : "",
        lifecycle,
        opened_at_movement_seq: record.opened_at_movement_seq,
        last_activity_movement_seq: record.last_activity_movement_seq,
        projection_kind: "worker_environment.formation.v5",
      });
    }
    return rows.sort((left, right) => (
      numberValue(right.last_activity_movement_seq, 0) - numberValue(left.last_activity_movement_seq, 0)
      || stringValue(left.worker_environment_id).localeCompare(stringValue(right.worker_environment_id))
    ));
  }

  async function projectedWorkerEnvironmentSurfaces(input) {
    const { formation, interfaceVarDir, kindCapabilities, lifecycle, liveState, openedAtMovementSeq, lastActivityMovementSeq, warnings } = input;
    const id = stringValue(formation.worker_environment_id);
    const workerEnvironmentKind = stringValue(formation.worker_environment_kind);
    const viewer = objectOrEmpty(liveState.viewer);
    const protocol = stringValue(viewer.protocol).toLowerCase();
    const address = stringValue(viewer.address);
    const status = lifecycle === "active" ? stringValue(liveState.status) || "not projected" : lifecycle;
    const rootBindings = Object.entries(objectOrEmpty(formation.door_root_bindings));
    const workspace = workerEnvironmentWorkspace(liveState, rootBindings);
    const connection = {
      id: `${id}:connection`,
      kind: "connection",
      label: "Connection",
      mode: "evidence",
      status,
      protocol: "worker_environment.formation.v5",
      read_only: true,
      data: {
        name: id,
        provider: stringValue(formation.door_interface),
        address,
        transport: protocol,
        workspace,
        persistence: formation.requires_release === true ? "Explicit release" : "Runtime lifecycle",
        state: status,
        sync: Number.isFinite(liveState.generation) ? `Generation ${liveState.generation}` : "Not projected",
        health: stringValue(liveState.detail) || (status === "error" || status === "failed" ? "Failed" : "Projected"),
        capabilities: workerEnvironmentCapabilities(formation, kindCapabilities),
        events: [
          { state: "active", label: "Environment formed", detail: `Movement ${openedAtMovementSeq}`, time: `seq ${openedAtMovementSeq}` },
          { state: status, label: lifecycle === "active" ? "Last activity" : `Environment ${lifecycle}`, detail: stringValue(liveState.detail), time: `seq ${lastActivityMovementSeq}` },
        ],
      },
    };
    const baseSurfaces = [
      connection,
      ...projectedFilesystemRootSurfaces({ formation, id, lifecycle, rootBindings, status }),
      ...await projectedGitSurfaces({ id, kindCapabilities, lifecycle, rootBindings, status }),
      ...projectedWorktreeStatusSurfaces({ id, liveState, status }),
    ];
    const activeCells = arrayValue(liveState.active_cells);
    const pendingToolCalls = arrayValue(liveState.pending_tool_calls);
    const runtimeDetail = stringValue(liveState.detail);
    if (!protocol && (activeCells.length || pendingToolCalls.length || runtimeDetail)) {
      return [...baseSurfaces, {
        id: `${id}:runtime-activity`, kind: "runtime", label: "Runtime activity", mode: "evidence", status,
        protocol: "worker_environment.live_state.v1", read_only: true,
        data: {
          active_cells: activeCells,
          pending_tool_calls: pendingToolCalls,
          detail: runtimeDetail,
        },
      }];
    }
    if (!protocol) return baseSurfaces;

    if (protocol === "browser.native.v1") {
      const engineHandle = stringValue(viewer.engine_handle);
      const hostGeneration = stringValue(viewer.host_generation);
      const browserContextId = stringValue(viewer.browser_context_id);
      const primaryTargetId = stringValue(viewer.target_id);
      let downloads = null;
      try {
        if (!store.nativeBrowserDownloadsForProjection) throw new Error("The native download history host is not connected.");
        const history = await store.nativeBrowserDownloadsForProjection({ octopusHome: input.octopusHomeRoot,
          worker_environment_id: id, formation_pin: stringValue(formation.formation_pin) });
        const historySupported = history?.engine?.capabilities?.persistentDownloadHistory;
        if (typeof historySupported !== "boolean") throw new Error("The native download history host returned no history capability declaration.");
        if (historySupported) {
          if (!stringValue(history.host_generation) || !history.downloads || history.downloads.available === false) throw new Error("The native download history host returned no exact available history owner.");
          downloads = { id: `${id}:downloads`, kind: "downloads", label: "Downloads", mode: "direct", status,
            protocol: "browser.native.downloads.v1", read_only: false,
            data: { formation_pin: stringValue(formation.formation_pin), host_generation: history.host_generation, downloads: history.downloads } };
        }
      } catch (error) { warnings.push(`Native browser download history ${id}: ${String(error?.message ?? error)}`); }
      const historySurfaces = () => downloads ? [downloads] : [];
      const unavailable = (reason, reasonCode = "unavailable") => {
        const recordedTabs = arrayValue(liveState.tabs);
        const ids = new Set();
        const exactTabs = browserContextId && recordedTabs.length && recordedTabs.every((tab) => {
          const targetId = stringValue(tab?.target_id);
          if (!targetId || ids.has(targetId) || stringValue(tab?.browser_context_id) !== browserContextId) return false;
          ids.add(targetId); return true;
        }) ? recordedTabs : [];
        const pages = exactTabs.length ? exactTabs : [null];
        return [...baseSurfaces, ...historySurfaces(), ...pages.map((tab) => ({
          ...unavailableViewerSurface({ id, label: "Browser", protocol, status: "unavailable", reason }),
          ...(tab ? { id: `${id}:browser:${encodeURIComponent(tab.target_id)}` } : {}),
          kind: "browser",
          label: (tab ? stringValue(tab.title) || stringValue(tab.url) : stringValue(liveState.title)) || "Browser",
          data: {
            detail: reason,
            reason_code: reasonCode,
            last_known_url: tab ? stringValue(tab.url) : stringValue(liveState.url),
            last_known_title: tab ? stringValue(tab.title) : stringValue(liveState.title),
          },
        }))];
      };
      if (lifecycle !== "active" || !engineHandle || !hostGeneration || !browserContextId || !stringValue(formation.formation_pin)) {
        return unavailable(lifecycle !== "active" ? `The browser room is ${lifecycle}; its native pages are no longer active.`
          : "The browser room has not projected its exact native engine and context.");
      }
      let observation;
      try {
        if (!store.nativeBrowserTargetsForProjection) throw new Error("The native browser host is not connected.");
        observation = await store.nativeBrowserTargetsForProjection({
          octopusHome: input.octopusHomeRoot,
          worker_environment_id: id, formation_pin: stringValue(formation.formation_pin),
          engine_handle: engineHandle, host_generation: hostGeneration, browser_context_id: browserContextId,
        });
      } catch (error) {
        return unavailable(String(error?.message ?? error), stringValue(error?.code) || "unavailable");
      }
      if (!observation || !Array.isArray(observation.targets)) return unavailable("The native browser host returned no page inventory.");
      const tabs = observation.targets;
      const targets = new Set();
      const validTabs = tabs.every((tab) => {
        const targetId = stringValue(tab?.target_id);
        if (!targetId || targets.has(targetId) || stringValue(tab?.browser_context_id) !== browserContextId) return false;
        targets.add(targetId);
        return true;
      });
      if (!validTabs) return unavailable("The native browser page inventory does not match its exact context.");
      if (downloads && downloads.data.host_generation === hostGeneration) Object.assign(downloads.data, { engine_handle: engineHandle, browser_context_id: browserContextId });
      if (!tabs.length) return [...baseSurfaces, ...historySurfaces()];
      const openIntent = observation.openIntent;
      if (openIntent != null && (!Number.isSafeInteger(openIntent.id) || openIntent.id <= 0
        || !targets.has(openIntent.targetId) || !["popup", "context-menu", "pip", "tool"].includes(openIntent.source) || typeof openIntent.activate !== "boolean")) {
        return unavailable("The native browser presentation request does not match its page inventory.");
      }
      return [...baseSurfaces, ...historySurfaces(), ...tabs.map((tab) => ({
        id: `${id}:browser:${encodeURIComponent(tab.target_id)}`,
        kind: "browser", label: stringValue(tab.title) || stringValue(tab.url) || "Browser", mode: "direct", status,
        protocol, read_only: false,
        data: {
          formation_pin: stringValue(formation.formation_pin),
          engine_handle: engineHandle,
          host_generation: hostGeneration,
          browser_context_id: browserContextId,
          target_id: tab.target_id,
          primary_target_id: primaryTargetId,
          observation_source: "client.native_browser",
          downloads: observation.downloads ?? null,
          native_open_intent: openIntent?.targetId === tab.target_id ? openIntent : null,
          browser_preview: typeof store.options.nativeBrowserPresentationsForProjection === "function" ? store.options.nativeBrowserPresentationsForProjection(id).find(value => value.target_id === tab.target_id) ?? null : null,
          presentation_intent: liveState.presentation_intent?.target_id === tab.target_id ? liveState.presentation_intent : null,
          opened_at_movement_seq: openedAtMovementSeq,
          url: stringValue(tab.url),
          title: stringValue(tab.title),
          lifetime: tab.lifetime,
          model_controlled: tab.model_controlled,
          disposition: tab.disposition,
          activity: tab.target_id === primaryTargetId ? liveState.activity ?? null : null,
          tabs,
          last_capture: tab.target_id === primaryTargetId ? liveState.last_capture ?? null : null,
        },
      }))];
    }

    if (protocol === "cdp") {
      if (lifecycle !== "active" || !stringValue(viewer.target_id ?? liveState.target_id) || !validAddressWithProtocols(address, ["http:", "https:", "ws:", "wss:"])) {
        return [...baseSurfaces, unavailableViewerSurface({
          id,
          label: "Browser",
          protocol,
          reason: lifecycle !== "active"
            ? `The browser room is ${lifecycle}; its interactive viewer is no longer active.`
            : "The browser room must project an exact target and a supported CDP endpoint.",
          status,
        })];
      }
      return [...baseSurfaces, {
        id: `${id}:browser`, kind: "browser", label: "Browser", mode: "direct", status,
        viewer_url: address, protocol, read_only: false,
        data: {
          url: stringValue(liveState.url),
          title: stringValue(liveState.title),
          target_id: stringValue(viewer.target_id ?? liveState.target_id),
          opened_at_movement_seq: openedAtMovementSeq,
          activity: liveState.activity ?? null,
          tabs: arrayValue(liveState.tabs),
          last_capture: liveState.last_capture ?? null,
        },
      }];
    }
    if (protocol === "desktop_session") {
      if (!["active", "failed"].includes(lifecycle) || !validHttpAddress(address)) {
        return [...baseSurfaces, unavailableViewerSurface({
          id,
          label: "Desktop",
          protocol,
          reason: !["active", "failed"].includes(lifecycle)
            ? `The desktop room is ${lifecycle}; its interactive viewer is no longer active.`
            : "The projected desktop viewer is not an embeddable absolute HTTP(S) URL.",
          status,
        })];
      }
      return [...baseSurfaces, {
        id: `${id}:desktop`, kind: "desktop", label: "Desktop", mode: "direct", status,
        viewer_url: address, protocol, read_only: false,
        data: {
          detail: stringValue(liveState.detail),
          desktop: liveState.desktop ?? null,
          opened_at_movement_seq: openedAtMovementSeq,
          last_activity_movement_seq: lastActivityMovementSeq,
          formation_pin: stringValue(formation.formation_pin),
          host_generation: stringValue(viewer.host_generation),
          native_presentations: typeof store.options.nativeDesktopPresentationsForProjection === "function" ? store.options.nativeDesktopPresentationsForProjection(id) : [],
        },
      }];
    }
    if (protocol === "remote_exec_server") {
      return [...baseSurfaces, {
        id: `${id}:remote-exec`, kind: "runtime", label: "Remote environment", mode: "evidence", status,
        protocol, read_only: true,
        data: {
          detail: "The projected endpoint speaks the structured exec-server protocol. An interactive terminal requires a separate xterm.websocket surface descriptor.",
          server: {
            transport: "Exec-server JSON-RPC",
            address,
            workspace,
          },
        },
      }];
    }
    if (protocol === "xterm.websocket") {
      if (lifecycle !== "active" || !validWebSocketAddress(address)) {
        return [...baseSurfaces, unavailableViewerSurface({
          id,
          label: "Terminal",
          protocol,
          reason: lifecycle !== "active"
            ? `The terminal room is ${lifecycle}; its interactive stream is no longer active.`
            : "The projected terminal stream is not an absolute WebSocket URL.",
          status,
        })];
      }
      return [...baseSurfaces, {
        id: `${id}:terminal`, kind: "terminal", label: "Terminal", mode: "direct", status,
        viewer_url: address, protocol, read_only: false,
        data: { workspace },
      }];
    }
    if (protocol === "process_session.direct.v1") {
      const authFile = stringValue(viewer.auth_file);
      const tty = liveState.tty === true;
      const hasProcess = Boolean(stringValue(liveState.process_id));
      const initialShellAvailable = !hasProcess && viewer.start_new_shell_available === true;
      const validDirectCoordinates = workerEnvironmentKind === "process_session"
        && lifecycle === "active"
        && Number.isSafeInteger(liveState.generation)
        && liveState.generation >= 0
        && typeof liveState.tty === "boolean"
        && path.isAbsolute(address)
        && path.isAbsolute(authFile)
        && isPathInside(interfaceVarDir, address)
        && isPathInside(interfaceVarDir, authFile);
      let validDirectEndpoint = false;
      if (validDirectCoordinates) {
        try {
          const [socketStat, authStat, resolvedInterfaceVarDir, resolvedSocketParent, resolvedAuth] = await Promise.all([
            fs.lstat(address),
            fs.lstat(authFile),
            fs.realpath(interfaceVarDir),
            fs.realpath(path.dirname(address)),
            fs.realpath(authFile),
          ]);
          validDirectEndpoint = socketStat.isSocket()
            && !socketStat.isSymbolicLink()
            && authStat.isFile()
            && !authStat.isSymbolicLink()
            && isPathInside(resolvedInterfaceVarDir, resolvedSocketParent)
            && isPathInside(resolvedInterfaceVarDir, resolvedAuth);
        } catch {
          validDirectEndpoint = false;
        }
      }
      if (!validDirectEndpoint) {
        return [...baseSurfaces, unavailableViewerSurface({
          id,
          label: "Terminal",
          protocol,
          reason: "The process room did not project an exact private viewer endpoint.",
          status,
        })];
      }
      return [...baseSurfaces, {
        id: `${id}:terminal`, kind: "terminal", label: tty || !hasProcess ? "Terminal" : "Output", mode: "direct", status,
        viewer_url: address, protocol, read_only: !tty && !initialShellAvailable,
        data: {
          workspace,
          auth_file: authFile,
          worker_environment_id: id,
          generation: liveState.generation,
          process_id: stringValue(liveState.process_id),
          process_status: stringValue(liveState.status),
          tty,
          exit_code: Number.isSafeInteger(liveState.exit_code) ? liveState.exit_code : null,
          closed: liveState.closed === true,
          failure: stringValue(liveState.failure),
          warning: stringValue(viewer.warning),
          restart_available: viewer.restart_available === true,
          start_new_shell_available: viewer.start_new_shell_available === true,
          launch_label: stringValue(viewer.launch_label),
        },
      }];
    }
    if (protocol === "terminal_transcript") {
      const transcript = await projectedTextFile(address, interfaceVarDir, warnings, "terminal transcript");
      return [...baseSurfaces, {
        id: `${id}:terminal`, kind: "terminal", label: "Terminal", mode: "evidence", status,
        protocol, read_only: true,
        data: {
          lines: transcript ? transcript.split(/\r?\n/) : [],
          workspace,
          ...(transcript ? {} : { unavailable: "The projected transcript could not be read." }),
        },
      }];
    }
    if (protocol === "container_runtime") {
      const container = objectOrEmpty(liveState.container);
      return [...baseSurfaces, {
        id: `${id}:container`, kind: "container", label: "Runtime", mode: "evidence", status,
        protocol, read_only: true,
        data: {
          name: stringValue(container.name ?? viewer.container_name) || id,
          image: stringValue(container.image),
          state: stringValue(container.status) || status,
          running: container.running === true,
          health: stringValue(container.health),
          exit_code: Number.isSafeInteger(container.exit_code) ? container.exit_code : null,
          uptime: stringValue(container.uptime),
          processes: arrayValue(container.processes),
          ports: arrayValue(container.ports),
          mounts: arrayValue(container.mounts),
          raw: container,
        },
      }];
    }
    if (protocol === "mcp_stdio_room") {
      const eventLog = await projectedTextFile(stringValue(viewer.event_log), interfaceVarDir, warnings, "MCP event log");
      const stderrLog = await projectedTextFile(stringValue(viewer.stderr_log), interfaceVarDir, warnings, "MCP stderr log");
      return [...baseSurfaces, {
        id: `${id}:mcp`, kind: "runtime", label: "MCP server", mode: "evidence", status,
        protocol, read_only: true,
        data: {
          server: objectOrEmpty(liveState.server),
          event_log: eventLog,
          stderr_log: stderrLog,
          error: stringValue(liveState.error),
        },
      }];
    }
    if (["preview", "site_preview", "http_preview"].includes(protocol) && validHttpAddress(address)) {
      if (lifecycle !== "active") {
        return [...baseSurfaces, unavailableViewerSurface({
          id,
          label: "Preview",
          protocol,
          reason: `The preview room is ${lifecycle}; its interactive viewer is no longer active.`,
          status,
        })];
      }
      return [...baseSurfaces, {
        id: `${id}:preview`, kind: "preview", label: "Preview", mode: "direct", status,
        viewer_url: address, protocol, read_only: true,
        data: {
          title: stringValue(liveState.title) || "Application preview",
          detail: stringValue(liveState.detail),
        },
      }];
    }
    return [...baseSurfaces, {
      id: `${id}:runtime`, kind: "runtime", label: "Runtime", mode: "evidence", status,
      protocol, read_only: true, data: liveState,
    }];
  }

  return { projectedWorkerEnvironmentSurfaces, readWorkerEnvironmentRecords };
}
