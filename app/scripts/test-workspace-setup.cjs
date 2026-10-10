const assert = require("node:assert/strict");
const childProcess = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { createProjectionStore, mutateOctopusOsWorkspace } = require("../electron/projection.cjs");
const {
  initializeWorkspace,
  mutateWorkspaceProject,
  readWorkspaceSetupState,
  recoverWorkspaceTransactions,
  commitClientAuthoringTransaction,
} = require("../electron/workspaceSetup.cjs");

async function main() {
  await verifyTerminalBootstrapBoundary();
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-workspace-test-"));
  try {
    await verifyLinkedTaskRemoval(await fs.realpath(temporary));
    await verifyExistingRootAttachment(temporary);
    await verifyRootFirstSetup(temporary);
    await verifyFreshAgentRecipe(temporary);
    const fixtureHomeRoot = path.join(temporary, "home");
    const fixtureRegistryRoot = path.join(fixtureHomeRoot, "registry");
    await createFixture(fixtureHomeRoot, fixtureRegistryRoot);
    const octopusHomeRoot = await fs.realpath(fixtureHomeRoot);
    const registryRoot = await fs.realpath(fixtureRegistryRoot);
    const refusedHomeRoot = path.join(temporary, "refused-home");
    const refusedRegistryRoot = path.join(refusedHomeRoot, "registry");
    await createFixture(refusedHomeRoot, refusedRegistryRoot);
    const refusedHome = await fs.realpath(refusedHomeRoot);
    const refusedRegistry = await fs.realpath(refusedRegistryRoot);
    const refusedRequest = setupRequest("workspace_refused_12345678");
    const refusedProjectId = deterministicId("ee_project", refusedRequest.operationId, "product");
    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot: refusedHome,
        registryRoot: refusedRegistry,
        request: refusedRequest,
        planOperatorSurface: (input) => {
          if (input.request.clientSurfaceRole === "task" && input.request.eeId === refusedProjectId) {
            throw new Error("nested Project task topology refused");
          }
          return fixturePlan(refusedHome, refusedRegistry, input);
        },
      }),
      /nested Project task topology refused/,
    );
    assert.deepEqual(await fs.readdir(path.join(refusedHome, "state", "ee_instances")), ["ee_existence.json"]);
    assert.deepEqual(await fs.readdir(path.join(refusedHome, "state", "runs")), []);
    assert.deepEqual(await fs.readdir(path.join(refusedRegistry, "interface")), []);
    assert.equal((await readWorkspaceSetupState({ octopusHomeRoot: refusedHome, registryRoot: refusedRegistry })).status, "not_configured");
    const plannerCalls = [];
    const planOperatorSurface = async (input) => {
      plannerCalls.push({
        role: input.request.clientSurfaceRole,
        eeId: input.request.eeId,
      });
      return fixturePlan(octopusHomeRoot, registryRoot, input);
    };
    const request = setupRequest("workspace_setup_12345678");
    const progress = [];
    const first = await initializeWorkspace({
      octopusHomeRoot,
      registryRoot,
      request,
      planOperatorSurface,
      onProgress: (event) => progress.push(event),
    });
    assert.equal(first.changed, true);
    assert.equal(first.action, "initialize_workspace");
    assert.equal(first.projects.length, 3);
    assert.equal(plannerCalls.length, 4);
    assert.deepEqual(plannerCalls.map((call) => call.role), ["home", "task", "task", "task"]);
    assert.equal(plannerCalls[0].eeId, first.rootEeId);
    assert.deepEqual(
      plannerCalls.slice(1).map((call) => call.eeId).sort(),
      first.projects.map((project) => project.eeId).sort(),
    );
    assert.deepEqual(
      await fs.readdir(path.join(octopusHomeRoot, "state", "runs")),
      [`${first.homeRunId}.json`],
    );
    assert.equal(
      (await fs.readdir(path.join(registryRoot, "interface"))).some((entry) => entry.includes("project_preflight_")),
      false,
    );
    assert.equal(progress.at(-1).phase, "complete");
    const homeSidecarPath = path.join(registryRoot, "interface", first.homeInterfaceId, "octopus_os.json");
    const sidecar = JSON.parse(await fs.readFile(homeSidecarPath, "utf8"));
    assert.equal(sidecar.presentation.name, "Founder");
    assert.equal(sidecar.presentation.projects.length, 3);
    assert.equal(sidecar.task_authoring_profiles.length, 3);
    assert.equal(sidecar.task_authoring_profiles[0].worker_choice_interface, "system_worker_coder_max");
    const state = await readWorkspaceSetupState({ octopusHomeRoot, registryRoot });
    assert.equal(state.status, "configured");
    assert.equal(state.receipt.root_ee_id, first.rootEeId);
    const second = await initializeWorkspace({ octopusHomeRoot, registryRoot, request, planOperatorSurface });
    assert.equal(second.changed, false);
    assert.equal(second.receiptDigest, first.receiptDigest);
    assert.equal(plannerCalls.length, 4);
    const voiceContext = { run_id: "voice_run", control_interface_id: "voice_control", provider_id: "voice_provider", delegation_interface_id: "voice_return" };
    sidecar.voice_context = voiceContext;
    sidecar.authoring_profile.payload_artifact_bindings = [{ id: "attachment:project-context", content: Array(32).fill(7) }];
    await fs.writeFile(homeSidecarPath, `${JSON.stringify(sidecar, null, 2)}\n`);
    const configuredRootPath = path.join(octopusHomeRoot, "state", "ee_instances", `${first.rootEeId}.json`);
    const configuredRoot = JSON.parse(await fs.readFile(configuredRootPath, "utf8"));
    configuredRoot.config = { octopus_os: { application_defaults: ["world_provider_codex"], driver_defaults: ["git_worktree"] } };
    configuredRoot.policy_narrowing = { allowed_worker_environment_kinds: ["git_worktree"] };
    await fs.writeFile(configuredRootPath, `${JSON.stringify(configuredRoot, null, 2)}\n`);
    let projectScope = await fixtureProjectScope(octopusHomeRoot, registryRoot, first);
    await fs.writeFile(homeSidecarPath, `${JSON.stringify({ ...sidecar, voice_context: { ...voiceContext, provider_id: "../escape" } }, null, 2)}\n`);
    const invalidVoiceScope = await fixtureProjectScope(octopusHomeRoot, registryRoot, first);
    await assert.rejects(mutateWorkspaceProject({
      octopusHomeRoot, registryRoot, scope: invalidVoiceScope,
      request: { action: "update_project_presentation", operationId: "invalid_voice_12345678", workspace: fixtureWorkspaceIdentity(invalidVoiceScope), projects: sidecar.presentation.projects.map(({ ee_id, ...presentation }) => ({ eeId: ee_id, ...presentation })) },
    }), /Home voice context must declare exact/);
    await fs.writeFile(homeSidecarPath, `${JSON.stringify(sidecar, null, 2)}\n`);
    const preflightEes = (await fs.readdir(path.join(octopusHomeRoot, "state", "ee_instances"))).sort();
    const preflightSidecar = await fs.readFile(path.join(registryRoot, "interface", first.homeInterfaceId, "octopus_os.json"), "utf8");
    await assert.rejects(
      mutateWorkspaceProject({
        octopusHomeRoot,
        registryRoot,
        scope: projectScope,
        planOperatorSurface: async () => {
          throw new Error("formed policy refuses the authored worker");
        },
        request: {
          action: "create_project",
          operationId: "project_refused_12345678",
          workspace: fixtureWorkspaceIdentity(projectScope),
          parentEeId: first.rootEeId,
          expectedParentDigest: projectScope.environments.find((environment) => environment.eeId === first.rootEeId).recordDigest,
          templateName: "project",
          name: "Refused",
          goal: "Refused",
          presentation: { icon: "circle", color: "gray" },
        },
      }),
      /formed policy refuses the authored worker/,
    );
    assert.deepEqual((await fs.readdir(path.join(octopusHomeRoot, "state", "ee_instances"))).sort(), preflightEes);
    assert.equal(
      await fs.readFile(path.join(registryRoot, "interface", first.homeInterfaceId, "octopus_os.json"), "utf8"),
      preflightSidecar,
    );
    const createdProject = await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope: projectScope,
      planOperatorSurface,
      request: {
        action: "create_project",
        operationId: "project_create_12345678",
        workspace: fixtureWorkspaceIdentity(projectScope),
        parentEeId: first.rootEeId,
        expectedParentDigest: projectScope.environments.find((environment) => environment.eeId === first.rootEeId).recordDigest,
        templateName: "project",
        name: "Research",
        goal: "Research",
        presentation: { icon: "flask", color: "blue" },
      },
    });
    assert.equal(createdProject.changed, true);
    assert.equal(createdProject.parentEeId, first.rootEeId);
    const createdRecord = JSON.parse(await fs.readFile(path.join(octopusHomeRoot, "state", "ee_instances", `${createdProject.eeId}.json`), "utf8"));
    assert.equal(createdRecord.template_name, "project");
    assert.deepEqual(createdRecord.config, configuredRoot.config);
    assert.equal(createdRecord.policy_narrowing, undefined);
    let updatedSidecar = JSON.parse(await fs.readFile(homeSidecarPath, "utf8"));
    assert.deepEqual(updatedSidecar.voice_context, voiceContext);
    assert.equal(updatedSidecar.presentation.projects.at(-1).ee_id, createdProject.eeId);
    assert.equal(updatedSidecar.task_authoring_profiles.at(-1).ee_id, createdProject.eeId);
    assert.deepEqual(updatedSidecar.task_authoring_profiles.at(-1).payload_artifact_bindings, sidecar.authoring_profile.payload_artifact_bindings);
    const repeatedCreate = await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope: projectScope,
      request: {
        action: "create_project",
        operationId: "project_create_12345678",
        workspace: fixtureWorkspaceIdentity(projectScope),
        parentEeId: first.rootEeId,
        expectedParentDigest: projectScope.environments.find((environment) => environment.eeId === first.rootEeId).recordDigest,
        templateName: "project",
        name: "Research",
        goal: "Research",
        presentation: { icon: "flask", color: "blue" },
      },
    });
    assert.equal(repeatedCreate.changed, false);
    assert.equal(repeatedCreate.eeId, createdProject.eeId);
    delete updatedSidecar.presentation.name;
    await fs.writeFile(homeSidecarPath, `${JSON.stringify(updatedSidecar, null, 2)}\n`);
    projectScope = await fixtureProjectScope(octopusHomeRoot, registryRoot, first);
    const presentations = updatedSidecar.presentation.projects.map((row) => ({
      eeId: row.ee_id,
      name: row.ee_id === createdProject.eeId ? "Research Lab" : row.name,
      icon: row.icon,
      color: row.color,
    })).reverse();
    const receiptPath = path.join(
      octopusHomeRoot,
      "client-state",
      "workspace",
      "topology-receipts",
      "project_present_12345678.json",
    );
    const originalLink = fs.link;
    let refusedReceiptOnce = false;
    fs.link = async (source, destination) => {
      if (!refusedReceiptOnce && destination === receiptPath) {
        refusedReceiptOnce = true;
        throw new Error("injected receipt publication failure");
      }
      return originalLink(source, destination);
    };
    try {
      const recoveredPresentation = await mutateWorkspaceProject({
        octopusHomeRoot,
        registryRoot,
        scope: projectScope,
        request: {
          action: "update_project_presentation",
          operationId: "project_present_12345678",
          workspace: fixtureWorkspaceIdentity(projectScope),
          projects: presentations,
        },
      });
      assert.equal(recoveredPresentation.changed, true);
      assert.equal(refusedReceiptOnce, true);
    } finally {
      fs.link = originalLink;
    }
    assert.equal(JSON.parse(await fs.readFile(receiptPath, "utf8")).action, "update_project_presentation");
    updatedSidecar = JSON.parse(await fs.readFile(homeSidecarPath, "utf8"));
    assert.equal(updatedSidecar.presentation.name, undefined);
    assert.deepEqual(updatedSidecar.voice_context, voiceContext);
    assert.deepEqual(updatedSidecar.presentation.projects.map((row) => row.ee_id), presentations.map((row) => row.eeId));
    assert.equal(updatedSidecar.presentation.projects.find((row) => row.ee_id === createdProject.eeId).name, "Research Lab");
    projectScope = await fixtureProjectScope(octopusHomeRoot, registryRoot, first);
    const projectDigest = projectScope.environments.find((environment) => environment.eeId === createdProject.eeId).recordDigest;
    const lateRunPath = path.join(octopusHomeRoot, "state", "runs", "run_project_delete_guard.json");
    await fs.writeFile(lateRunPath, `${JSON.stringify({
      id: "run_project_delete_guard",
      ee: createdProject.eeId,
    }, null, 2)}\n`);
    await assert.rejects(
      mutateWorkspaceProject({
        octopusHomeRoot,
        registryRoot,
        scope: projectScope,
        request: {
          action: "delete_project",
          operationId: "project_delete_12345678",
          workspace: fixtureWorkspaceIdentity(projectScope),
          eeId: createdProject.eeId,
          expectedProjectDigest: projectDigest,
        },
      }),
      /is still referenced by run 'run_project_delete_guard'/,
    );
    await fs.unlink(lateRunPath);
    const deletedProject = await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope: projectScope,
      request: {
        action: "delete_project",
        operationId: "project_delete_12345678",
        workspace: fixtureWorkspaceIdentity(projectScope),
        eeId: createdProject.eeId,
        expectedProjectDigest: projectDigest,
      },
    });
    assert.equal(deletedProject.changed, true);
    await assert.rejects(fs.readFile(path.join(octopusHomeRoot, "state", "ee_instances", `${createdProject.eeId}.json`)), /ENOENT/);
    updatedSidecar = JSON.parse(await fs.readFile(path.join(registryRoot, "interface", first.homeInterfaceId, "octopus_os.json"), "utf8"));
    assert.equal(updatedSidecar.presentation.projects.some((row) => row.ee_id === createdProject.eeId), false);
    assert.equal(updatedSidecar.task_authoring_profiles.some((row) => row.ee_id === createdProject.eeId), false);
    assert.deepEqual(updatedSidecar.voice_context, voiceContext);
    const repeatedDelete = await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope: projectScope,
      request: {
        action: "delete_project",
        operationId: "project_delete_12345678",
        workspace: fixtureWorkspaceIdentity(projectScope),
        eeId: createdProject.eeId,
        expectedProjectDigest: projectDigest,
      },
    });
    assert.equal(repeatedDelete.changed, false);
    const externalEeId = "ee_created_outside_client";
    const externalEePath = path.join(octopusHomeRoot, "state", "ee_instances", `${externalEeId}.json`);
    const rootRecord = JSON.parse(await fs.readFile(path.join(octopusHomeRoot, "state", "ee_instances", `${first.rootEeId}.json`), "utf8"));
    await fs.writeFile(externalEePath, JSON.stringify({ ...rootRecord, ee_id: externalEeId, parent_ee_id: first.rootEeId, template_name: "project", goal: "Model-created goal" }));
    const retainedSidecar = await fs.readFile(homeSidecarPath, "utf8");
    const agentCatalogFile = path.join(octopusHomeRoot, "client", "agents", "catalog.json");
    const retainedCatalog = await fs.readFile(agentCatalogFile, "utf8");
    projectScope = await fixtureProjectScope(octopusHomeRoot, registryRoot, first);
    const externalDeleted = await mutateWorkspaceProject({
      octopusHomeRoot, registryRoot, scope: projectScope,
      request: { action: "delete_project", operationId: "external_delete_12345678", workspace: fixtureWorkspaceIdentity(projectScope),
        eeId: externalEeId, expectedProjectDigest: projectScope.environments.find((ee) => ee.eeId === externalEeId).recordDigest },
    });
    assert.equal(externalDeleted.changed, true);
    await assert.rejects(fs.access(externalEePath), { code: "ENOENT" });
    assert.equal(await fs.readFile(homeSidecarPath, "utf8"), retainedSidecar);
    assert.equal(await fs.readFile(agentCatalogFile, "utf8"), retainedCatalog);
    assert.equal((await readWorkspaceSetupState({ octopusHomeRoot, registryRoot })).status, "configured");
    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot,
        registryRoot,
        request: setupRequest("workspace_second_12345678"),
        planOperatorSurface,
      }),
      /already configured receipt root/,
    );
    const homeDescriptorPath = path.join(registryRoot, "interface", first.homeInterfaceId, "interface.json");
    const homeDescriptor = await fs.readFile(homeDescriptorPath);
    const symlinkTarget = path.join(temporary, "symlink-interface.json");
    await fs.writeFile(symlinkTarget, homeDescriptor);
    await fs.unlink(homeDescriptorPath);
    await fs.symlink(symlinkTarget, homeDescriptorPath);
    const symlinkState = await readWorkspaceSetupState({ octopusHomeRoot, registryRoot });
    assert.equal(symlinkState.status, "degraded");
    assert.match(symlinkState.errors.join("\n"), /ELOOP|symbolic link/i);
    await fs.unlink(homeDescriptorPath);
    await fs.writeFile(homeDescriptorPath, homeDescriptor);

    const progressFailureHome = path.join(temporary, "progress-failure-home");
    const progressFailureRegistry = path.join(progressFailureHome, "registry");
    await createFixture(progressFailureHome, progressFailureRegistry);
    const progressFailureHomeRoot = await fs.realpath(progressFailureHome);
    const progressFailureRegistryRoot = await fs.realpath(progressFailureRegistry);
    const progressFailure = await initializeWorkspace({
      octopusHomeRoot: progressFailureHomeRoot,
      registryRoot: progressFailureRegistryRoot,
      request: setupRequest("workspace_progress_12345678"),
      planOperatorSurface: (input) => fixturePlan(progressFailureHomeRoot, progressFailureRegistryRoot, input),
      onProgress: (event) => {
        if (event.phase === "complete") throw new Error("detached progress listener");
      },
    });
    assert.equal(progressFailure.changed, true);
    assert.equal(
      JSON.parse(await fs.readFile(path.join(progressFailureHomeRoot, "state", "runs", `${progressFailure.homeRunId}.json`), "utf8")).id,
      progressFailure.homeRunId,
    );
    assert.equal(
      (await readWorkspaceSetupState({ octopusHomeRoot: progressFailureHomeRoot, registryRoot: progressFailureRegistryRoot })).status,
      "configured",
    );

    const abandonedStageRoot = path.join(
      progressFailureHomeRoot,
      "client-state",
      "workspace",
      "transactions",
      ".stage-workspace_stage_12345678-0123456789abcdef",
    );
    await fs.mkdir(abandonedStageRoot);
    assert.equal(
      (await readWorkspaceSetupState({ octopusHomeRoot: progressFailureHomeRoot, registryRoot: progressFailureRegistryRoot })).status,
      "configured",
    );
    await assert.rejects(fs.lstat(abandonedStageRoot), /ENOENT/);
    const mutableRootPath = path.join(
      progressFailureHomeRoot,
      "state",
      "ee_instances",
      `${progressFailure.rootEeId}.json`,
    );
    const mutableRoot = JSON.parse(await fs.readFile(mutableRootPath, "utf8"));
    mutableRoot.goal = "A legitimately edited operator goal";
    await fs.writeFile(mutableRootPath, `${JSON.stringify(mutableRoot, null, 2)}\n`);
    assert.equal(
      (await readWorkspaceSetupState({
        octopusHomeRoot: progressFailureHomeRoot,
        registryRoot: progressFailureRegistryRoot,
      })).status,
      "configured",
    );

    const conflictFixtureHome = path.join(temporary, "conflict-home");
    const conflictFixtureRegistry = path.join(conflictFixtureHome, "registry");
    await createFixture(conflictFixtureHome, conflictFixtureRegistry);
    const conflictHomeRoot = await fs.realpath(conflictFixtureHome);
    const conflictRegistryRoot = await fs.realpath(conflictFixtureRegistry);
    const conflictRequest = setupRequest("workspace_conflict_12345678");
    let injectConflict = true;
    const conflictPlanner = async (input) => {
      const plan = await fixturePlan(conflictHomeRoot, conflictRegistryRoot, input);
      if (!injectConflict) return plan;
      injectConflict = false;
      const conflictPath = path.join(conflictRegistryRoot, "interface", plan.chatInterfaceId, "conflict.json");
      await fs.mkdir(path.dirname(conflictPath), { recursive: true });
      await fs.writeFile(conflictPath, "foreign\n");
      plan.writes.splice(1, 0, write(conflictPath, '{"planned":true}\n', "conflict"));
      return plan;
    };
    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot: conflictHomeRoot,
        registryRoot: conflictRegistryRoot,
        request: conflictRequest,
        planOperatorSurface: conflictPlanner,
      }),
      /expected an absent file, found/,
    );
    assert.deepEqual(await fs.readdir(path.join(conflictHomeRoot, "state", "ee_instances")), ["ee_existence.json"]);
    assert.equal(JSON.parse(await fs.readFile(path.join(conflictHomeRoot, "state", "ee_instances", "ee_existence.json"))).goal, "existence");
    const transactionEntries = await fs.readdir(path.join(conflictHomeRoot, "client-state", "workspace", "transactions"));
    assert.deepEqual(transactionEntries, []);

    const driftFixtureHome = path.join(temporary, "plan-drift-home");
    const driftFixtureRegistry = path.join(driftFixtureHome, "registry");
    await createFixture(driftFixtureHome, driftFixtureRegistry);
    const driftHomeRoot = await fs.realpath(driftFixtureHome);
    const driftRegistryRoot = await fs.realpath(driftFixtureRegistry);
    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot: driftHomeRoot,
        registryRoot: driftRegistryRoot,
        request: setupRequest("workspace_plan_drift_12345678"),
        planOperatorSurface: async (input) => {
          const plan = await fixturePlan(driftHomeRoot, driftRegistryRoot, input);
          plan.authoringProfile.external_inputs = [];
          return plan;
        },
      }),
      /changed the requested authoring profile/,
    );

    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot: driftHomeRoot,
        registryRoot: driftRegistryRoot,
        request: setupRequest("workspace_owner_drift_12345678"),
        planOperatorSurface: async (input) => {
          const plan = await fixturePlan(driftHomeRoot, driftRegistryRoot, input);
          const foreignOwner = "foreign_interface";
          plan.requiredInterfaceIds.push(foreignOwner);
          plan.requiredInterfaceIds.sort();
          plan.writes.push(write(
            path.join(driftRegistryRoot, "interface", foreignOwner, "interface.json"),
            `${JSON.stringify({ id: foreignOwner })}\n`,
            `interface:${foreignOwner}`,
          ));
          return plan;
        },
      }),
      /outside its active Home bindings or worker ceiling/,
    );
    for (const defect of ["disconnected", "inactive", "foreign-tool-door", "missing-descriptor"]) {
      await assert.rejects(initializeWorkspace({
        octopusHomeRoot: driftHomeRoot, registryRoot: driftRegistryRoot,
        request: setupRequest(`workspace_${defect.replaceAll("-", "_")}_12345678`),
        planOperatorSurface: async (input) => {
          const plan = await fixturePlan(driftHomeRoot, driftRegistryRoot, input);
          const runWrite = plan.writes.find((row) => row.tag.startsWith("run:"));
          const run = JSON.parse(runWrite.content);
          const intake = run.checkpoints.filter((row) => row.id.startsWith("cp_intake_"));
          const service = intake[0].interface_binding.interface;
          if (defect === "disconnected") intake[0].outbound_interface_bindings = intake[1].outbound_interface_bindings;
          if (defect === "inactive") run.active_checkpoints = run.active_checkpoints.filter((id) => !intake.some((row) => row.id === id));
          if (defect === "foreign-tool-door") {
            const tool = plan.writes.find((row) => row.tag.startsWith("tool:file.write.input_"));
            tool.content = JSON.stringify({ ...JSON.parse(tool.content), interface: "foreign_interface" });
          }
          if (defect === "missing-descriptor") plan.writes = plan.writes.filter((row) => row.filePath !== path.join(driftRegistryRoot, "interface", service, "interface.json"));
          runWrite.content = JSON.stringify(run);
          return plan;
        },
      }), /outside its active Home bindings or worker ceiling|omitted exact interface descriptor/);
      assert.deepEqual(await fs.readdir(path.join(driftHomeRoot, "state/runs")), []);
    }
    for (const defect of ["unbound", "unreadable", "wrong-tag", "non-object", "wrong-file", "changed-before"]) {
      await assert.rejects(initializeWorkspace({
        octopusHomeRoot: driftHomeRoot, registryRoot: driftRegistryRoot, request: setupRequest(`workspace_artifact_${defect.replaceAll("-", "_")}`),
        planOperatorSurface: async (input) => {
          const plan = await fixturePlan(driftHomeRoot, driftRegistryRoot, input);
          if (input.request.clientSurfaceRole !== "home") return plan;
          const artifact = plan.writes.find((write) => write.tag.startsWith("artifact:"));
          const runWrite = plan.writes.find((write) => write.tag.startsWith("run:")), run = JSON.parse(runWrite.content);
          if (defect === "unbound") run.checkpoints[0].artifact_bindings = [];
          if (defect === "unreadable") run.checkpoints[0].artifact_bindings[0].access = { read: false };
          if (defect === "wrong-tag") artifact.tag = "artifact:foreign";
          if (defect === "non-object") artifact.content = "[]";
          if (defect === "wrong-file") artifact.filePath = path.join(path.dirname(artifact.filePath), "other.json");
          if (defect === "changed-before") artifact.expectedBeforeHash = "a".repeat(64);
          runWrite.content = JSON.stringify(run);
          return plan;
        },
      }), /exact readable checkpoint binding|exact owner|artifact.json|absent fresh-workspace file|JSON object/);
      assert.deepEqual(await fs.readdir(path.join(driftRegistryRoot, "artifact")), []);
    }

    const orphanFixtureHome = path.join(temporary, "orphan-home");
    const orphanFixtureRegistry = path.join(orphanFixtureHome, "registry");
    await createFixture(orphanFixtureHome, orphanFixtureRegistry);
    const orphanHome = await fs.realpath(orphanFixtureHome);
    const orphanRegistry = await fs.realpath(orphanFixtureRegistry);
    await fs.writeFile(path.join(orphanHome, "state", "ee_instances", "ee_orphan_space.json"), `${JSON.stringify({
      ee_id: "ee_orphan_space",
      template_name: "operator_space",
      parent_ee_id: "ee_existence",
      goal: "Orphaned workspace",
      config: {},
    }, null, 2)}\n`);
    const orphanState = await readWorkspaceSetupState({ octopusHomeRoot: orphanHome, registryRoot: orphanRegistry });
    assert.equal(orphanState.status, "degraded");
    assert.match(orphanState.errors.join("\n"), /operator spaces without receipts/);
    await assert.rejects(
      initializeWorkspace({
        octopusHomeRoot: orphanHome,
        registryRoot: orphanRegistry,
        request: setupRequest("workspace_orphan_12345678"),
        planOperatorSurface: (input) => fixturePlan(orphanHome, orphanRegistry, input),
      }),
      /refuses existing operator space/,
    );

    const crashFixtureHome = path.join(temporary, "crash-home");
    const crashFixtureRegistry = path.join(crashFixtureHome, "registry");
    await createFixture(crashFixtureHome, crashFixtureRegistry);
    const crashHome = await fs.realpath(crashFixtureHome);
    const crashRegistry = await fs.realpath(crashFixtureRegistry);
    const child = childProcess.spawnSync(process.execPath, [__filename, "crash-child", crashHome, crashRegistry], {
      encoding: "utf8",
    });
    assert.equal(child.status, 73, child.stderr || child.stdout);
    const recoveredState = await readWorkspaceSetupState({ octopusHomeRoot: crashHome, registryRoot: crashRegistry });
    assert.equal(recoveredState.status, "configured");
    assert.equal(recoveredState.receipt.operation_id, "workspace_crash_12345678");
    assert.equal(recoveredState.receipt.root_ee_id, "ee_existence");
    assert.equal(JSON.parse(await fs.readFile(path.join(crashHome, "state", "ee_instances", "ee_existence.json"))).goal, "Operate life and work");
    assert.equal((await fs.readdir(path.join(crashHome, "client-state", "workspace", "transactions"))).length, 0);

    const projectCrashFixtureHome = path.join(temporary, "project-crash-home");
    const projectCrashFixtureRegistry = path.join(projectCrashFixtureHome, "registry");
    await createFixture(projectCrashFixtureHome, projectCrashFixtureRegistry);
    const projectCrashHome = await fs.realpath(projectCrashFixtureHome);
    const projectCrashRegistry = await fs.realpath(projectCrashFixtureRegistry);
    const projectCrashWorkspace = await initializeWorkspace({
      octopusHomeRoot: projectCrashHome,
      registryRoot: projectCrashRegistry,
      request: setupRequest("workspace_project_crash_12345678"),
      planOperatorSurface: (input) => fixturePlan(projectCrashHome, projectCrashRegistry, input),
    });
    const projectCrashArgs = [
      projectCrashHome,
      projectCrashRegistry,
      projectCrashWorkspace.rootEeId,
      projectCrashWorkspace.homeRunId,
      projectCrashWorkspace.homeInterfaceId,
    ];
    const createCrash = childProcess.spawnSync(
      process.execPath,
      [__filename, "project-crash-child", "create", ...projectCrashArgs],
      { encoding: "utf8" },
    );
    assert.equal(createCrash.status, 73, createCrash.stderr || createCrash.stdout);
    const crashCreatedEeId = deterministicId("ee_project", "project_crash_create_12345678", "project");
    assert.equal(
      JSON.parse(await fs.readFile(path.join(projectCrashHome, "state", "ee_instances", `${crashCreatedEeId}.json`), "utf8")).ee_id,
      crashCreatedEeId,
    );
    let projectCrashSidecar = JSON.parse(await fs.readFile(
      path.join(projectCrashRegistry, "interface", projectCrashWorkspace.homeInterfaceId, "octopus_os.json"),
      "utf8",
    ));
    assert.equal(projectCrashSidecar.task_authoring_profiles.some((profile) => profile.ee_id === crashCreatedEeId), false);
    assert.equal(projectCrashSidecar.presentation.projects.some((projectRow) => projectRow.ee_id === crashCreatedEeId), false);
    let projectCrashState = await readWorkspaceSetupState({
      octopusHomeRoot: projectCrashHome,
      registryRoot: projectCrashRegistry,
    });
    assert.equal(projectCrashState.status, "configured");
    assert.equal(
      JSON.parse(await fs.readFile(path.join(projectCrashHome, "state", "ee_instances", `${crashCreatedEeId}.json`), "utf8")).ee_id,
      crashCreatedEeId,
    );
    projectCrashSidecar = JSON.parse(await fs.readFile(
      path.join(projectCrashRegistry, "interface", projectCrashWorkspace.homeInterfaceId, "octopus_os.json"),
      "utf8",
    ));
    assert.equal(projectCrashSidecar.task_authoring_profiles.filter((profile) => profile.ee_id === crashCreatedEeId).length, 1);
    assert.equal(projectCrashSidecar.presentation.projects.filter((projectRow) => projectRow.ee_id === crashCreatedEeId).length, 1);
    assert.equal(
      JSON.parse(await fs.readFile(path.join(
        projectCrashHome,
        "client-state",
        "workspace",
        "topology-receipts",
        "project_crash_create_12345678.json",
      ), "utf8")).action,
      "create_project",
    );
    assert.deepEqual(await fs.readdir(path.join(projectCrashHome, "client-state", "workspace", "transactions")), []);

    const historicalProject = projectCrashWorkspace.projects.find((entry) => entry.key === "life");
    assert.ok(historicalProject);
    const deleteCrash = childProcess.spawnSync(
      process.execPath,
      [__filename, "project-crash-child", "delete", ...projectCrashArgs, historicalProject.eeId],
      { encoding: "utf8" },
    );
    assert.equal(deleteCrash.status, 73, deleteCrash.stderr || deleteCrash.stdout);
    assert.equal(
      JSON.parse(await fs.readFile(path.join(projectCrashHome, "state", "ee_instances", `${historicalProject.eeId}.json`), "utf8")).ee_id,
      historicalProject.eeId,
    );
    projectCrashSidecar = JSON.parse(await fs.readFile(
      path.join(projectCrashRegistry, "interface", projectCrashWorkspace.homeInterfaceId, "octopus_os.json"),
      "utf8",
    ));
    assert.equal(projectCrashSidecar.task_authoring_profiles.some((profile) => profile.ee_id === historicalProject.eeId), false);
    assert.equal(projectCrashSidecar.presentation.projects.some((projectRow) => projectRow.ee_id === historicalProject.eeId), false);
    projectCrashState = await readWorkspaceSetupState({
      octopusHomeRoot: projectCrashHome,
      registryRoot: projectCrashRegistry,
    });
    assert.equal(projectCrashState.status, "configured");
    assert.equal(
      projectCrashState.receipt.project_ee_ids.some((entry) => entry.ee_id === historicalProject.eeId),
      true,
    );
    await assert.rejects(
      fs.readFile(path.join(projectCrashHome, "state", "ee_instances", `${historicalProject.eeId}.json`)),
      /ENOENT/,
    );
    projectCrashSidecar = JSON.parse(await fs.readFile(
      path.join(projectCrashRegistry, "interface", projectCrashWorkspace.homeInterfaceId, "octopus_os.json"),
      "utf8",
    ));
    assert.equal(projectCrashSidecar.task_authoring_profiles.some((profile) => profile.ee_id === historicalProject.eeId), false);
    assert.equal(projectCrashSidecar.presentation.projects.some((projectRow) => projectRow.ee_id === historicalProject.eeId), false);
    assert.equal(
      JSON.parse(await fs.readFile(path.join(
        projectCrashHome,
        "client-state",
        "workspace",
        "topology-receipts",
        "project_crash_delete_12345678.json",
      ), "utf8")).action,
      "delete_project",
    );
    assert.deepEqual(await fs.readdir(path.join(projectCrashHome, "client-state", "workspace", "transactions")), []);

    const mismatchFixtureHome = path.join(temporary, "journal-mismatch-home");
    const mismatchFixtureRegistry = path.join(mismatchFixtureHome, "registry");
    await createFixture(mismatchFixtureHome, mismatchFixtureRegistry);
    const mismatchHome = await fs.realpath(mismatchFixtureHome);
    const mismatchRegistry = await fs.realpath(mismatchFixtureRegistry);
    const mismatchChild = childProcess.spawnSync(
      process.execPath,
      [__filename, "crash-child", mismatchHome, mismatchRegistry],
      { encoding: "utf8" },
    );
    assert.equal(mismatchChild.status, 73, mismatchChild.stderr || mismatchChild.stdout);
    const mismatchJournalPath = path.join(
      mismatchHome,
      "client-state",
      "workspace",
      "transactions",
      "workspace_crash_12345678",
      "journal.json",
    );
    const mismatchJournal = JSON.parse(await fs.readFile(mismatchJournalPath, "utf8"));
    mismatchJournal.receipt.writes[0].digest = "0".repeat(64);
    await fs.writeFile(mismatchJournalPath, `${JSON.stringify(mismatchJournal, null, 2)}\n`);
    const mismatchState = await readWorkspaceSetupState({
      octopusHomeRoot: mismatchHome,
      registryRoot: mismatchRegistry,
    });
    assert.equal(mismatchState.status, "degraded");
    assert.match(mismatchState.errors.join("\n"), /receipt disagrees with its journal writes/);

    const driftedRootPath = mutableRootPath;
    await fs.writeFile(driftedRootPath, "{}\n");
    const driftedState = await readWorkspaceSetupState({
      octopusHomeRoot: progressFailureHomeRoot,
      registryRoot: progressFailureRegistryRoot,
    });
    assert.equal(driftedState.status, "degraded");
    assert.equal(driftedState.receipt, null);
    assert.match(driftedState.errors.join("\n"), /root EE identity/);
    await verifyOperatorChatStatusProjection(temporary);
    await verifyTaskArchive(temporary);
    await verifyProjectionReadinessBarrier(temporary);
  } finally {
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

async function verifyLinkedTaskRemoval(temporary) {
  const home = path.join(temporary, "task-removal"), registry = path.join(home, "registry");
  const run = path.join(home, "state/runs/task.json"), owned = path.join(home, "interfaces/provider");
  const target = path.join(temporary, "shared-auth"), content = "shared account state\n";
  await createFixture(home, registry);
  await fs.mkdir(path.dirname(run), { recursive: true });
  await fs.mkdir(path.join(owned, "app-server-home"), { recursive: true });
  await fs.writeFile(run, "{}\n");
  await fs.writeFile(target, content);
  await fs.symlink(target, path.join(owned, "app-server-home/auth.json"));
  await fs.symlink(`${target}-missing`, path.join(owned, "dangling"));
  const stat = await fs.lstat(owned);
  const options = { octopusHomeRoot: home, registryRoot: registry, plan: {
    operationId: "task_remove_links_12345678", label: "delete task",
    writes: [{ destination: run, expectedBeforeDigest: crypto.createHash("sha256").update("{}\n").digest("hex"), content: null, tag: "run", publishBarrier: true }],
    removals: [{ destination: owned, type: "directory", expectedDev: stat.dev, expectedIno: stat.ino, tag: "private state" }],
  } };
  await commitClientAuthoringTransaction(options);
  await recoverWorkspaceTransactions(options);
  await commitClientAuthoringTransaction(options);
  await assert.rejects(fs.lstat(run), /ENOENT/);
  await assert.rejects(fs.lstat(owned), /ENOENT/);
  assert.equal(await fs.readFile(target, "utf8"), content);
}

async function verifyOperatorChatStatusProjection(temporary) {
  const fixtureHome = path.join(temporary, "operator-status-home");
  const fixtureRegistry = path.join(fixtureHome, "registry");
  await createFixture(fixtureHome, fixtureRegistry);
  const octopusHomeRoot = await fs.realpath(fixtureHome);
  const registryRoot = await fs.realpath(fixtureRegistry);
  const workspace = await initializeWorkspace({
    octopusHomeRoot,
    registryRoot,
    request: setupRequest("workspace_status_12345678"),
    planOperatorSurface: (input) => fixturePlan(octopusHomeRoot, registryRoot, input),
  });
  const protocolRoot = path.join(registryRoot, "interface", "world_operator_chat");
  await fs.mkdir(protocolRoot, { recursive: true });
  await fs.copyFile(
    path.join(__dirname, "../../registry/interface/world_operator_chat/client.json"),
    path.join(protocolRoot, "client.json"),
  );
  const ledgerRoot = path.join(octopusHomeRoot, "ledger");
  const ledgerPath = path.join(ledgerRoot, "movements.jsonl");
  await fs.mkdir(ledgerRoot, { recursive: true });
  await fs.writeFile(ledgerPath, "", "utf8");
  const ledgerIndexAvailable = createFixtureLedgerIndex(path.join(ledgerRoot, "ledger-index.sqlite"));
  const policyProjection = await fixturePolicyProjection(octopusHomeRoot);
  const store = createProjectionStore({
    resolveRepoRoot: () => path.resolve(__dirname, "../.."),
    sourceRegistry: () => registryRoot,
    octopusHome: () => octopusHomeRoot,
    projectEePolicies: async () => policyProjection,
  });
  const readStatuses = () => store.read({
    contract_version: 1,
    method: "projection.operator.chats.status",
    params: { run_ids: [workspace.homeRunId] },
  });
  try {
    const initial = await store.read({
      contract_version: 1,
      method: "projection.operator.chats.list",
    });
    const initialHomeChat = initial.chats.find((chat) => chat.run_id === workspace.homeRunId);
    assert.equal(initialHomeChat?.turn_state, "idle");
    assert.equal(
      initialHomeChat?.provider_interface_id,
      setupRequest("workspace_status_12345678").home.providerInterfaceId,
    );
    assert.equal(
      initialHomeChat?.provider_bound_interface_id,
      `${setupRequest("workspace_status_12345678").home.providerInterfaceId}_${workspace.homeRunId.replace("run_operator_chat_", "")}`,
    );
    const provider = initialHomeChat.provider_bound_interface_id;
    const workingMovements = [
      fixtureMovement(0, workspace, "operator.message", workspace.homeInterfaceId, "run"),
      fixtureMovement(1, workspace, "turn.started", `${setupRequest("workspace_status_12345678").home.providerInterfaceId}_${workspace.homeRunId.replace("run_operator_chat_", "")}`, "run"),
    ];
    await fs.appendFile(ledgerPath, workingMovements.map((movement) => `${JSON.stringify(movement)}\n`).join(""));
    if (ledgerIndexAvailable) await indexFixtureMovements(ledgerRoot, workingMovements);
    const working = await readStatuses();
    assert.deepEqual(working.chats.map((chat) => ({
      run_id: chat.run_id,
      turn_open: chat.turn_open,
      turn_state: chat.turn_state,
      last_movement_seq: chat.last_movement_seq,
    })), [{
      run_id: workspace.homeRunId,
      turn_open: true,
      turn_state: "working",
      last_movement_seq: 1,
    }]);
    await fs.appendFile(
      ledgerPath,
      `${JSON.stringify(fixtureMovement(2, workspace, "interface.artifact.output", provider, "run"))}\n`,
    );
    const idle = await readStatuses();
    assert.equal(idle.chats[0].turn_open, false);
    assert.equal(idle.chats[0].turn_state, "idle");
    assert.equal(idle.chats[0].last_movement_seq, 2);
    assert.equal((await readStatuses()).ledger_cursor_seq, 2);
    const cases = [
      ["turn.started", "other-provider", {}, "idle"],
      ["provider.peer.input", "peer", { message: { action: "queue" } }, "idle"],
      ["provider.transport.dispatch", provider, { provider_interface: provider, method: "thread/injectItems" }, "idle"],
      ["provider.peer.input", "peer", { message: { action: "followup" } }, "waiting"],
      ["provider.transport.dispatch", provider, { provider_interface: provider, method: "turn/start" }, "working"],
      ["turn.reasoning", provider, { lifecycle: "completed" }, "working"],
      ["interface.artifact.output", "other-provider", {}, "working"],
      ["turn.error", provider, { will_retry: true }, "working"],
      [`${provider}.failure`, provider, { failure_class: "dispatch_failed", source_movement_seq: 6 }, "failed"],
      ["turn.started", provider, {}, "working"],
      ["turn.completed", provider, {}, "idle"],
      ["account.rate_limits", provider, {}, "idle"],
      ["provider.transport.dispatch", provider, { provider_interface: provider, method: "turn/start" }, "working"],
      ["interface.artifact.output", provider, {}, "idle"],
      ["turn.started", provider, {}, "working"],
      ["turn.error", provider, { will_retry: false }, "failed"],
      ["turn.started", provider, {}, "working"],
      ["turn.interrupted.result", provider, {}, "idle"],
      ["turn.started", provider, {}, "working"],
      ["turn.control.acknowledged", provider, { action: "interrupt", status: "acknowledged", control_id: "stop" }, "idle"],
      ["turn.started", provider, {}, "working"],
      ["turn.completed", provider, { status: "failed" }, "failed"],
    ];
    await fs.mkdir(path.join(ledgerRoot, "blobs"), { recursive: true });
    for (const [offset, [label, from, payload, expected]] of cases.entries()) {
      const movement = fixtureMovement(offset + 3, workspace, label, from, "run");
      if (from === "peer" || label === `${provider}.failure`) {
        movement.kind = "InterfaceOutbound";
        movement.to = { Interface: from === "peer" ? provider : workspace.homeInterfaceId };
      }
      const bytes = Buffer.from(JSON.stringify(payload)), hash = Buffer.from(require("@noble/hashes/blake3").blake3(bytes));
      movement.artifact.content = [...hash];
      movement.recorded_at_unix_ms -= offset * 2;
      await fs.writeFile(path.join(ledgerRoot, "blobs", hash.toString("hex")), bytes);
      await fs.appendFile(ledgerPath, `${JSON.stringify(movement)}\n`);
      const status = (await readStatuses()).chats[0];
      assert.deepEqual([status.turn_state, status.turn_open], [expected, ["waiting", "working"].includes(expected)], label);
      store.invalidate();
      const cold = (await store.read({ contract_version: 1, method: "projection.operator.chats.list" })).chats.find((chat) => chat.run_id === workspace.homeRunId);
      assert.deepEqual([cold.turn_state, cold.turn_open], [status.turn_state, status.turn_open], `replay: ${label}`);
    }
    const retired = fixtureMovement(cases.length + 3, workspace, "turn.started", provider, "run");
    await fs.appendFile(ledgerPath, `${JSON.stringify(retired)}\n`);
    assert.equal((await readStatuses()).chats[0].turn_state, "working");
    const runPath = path.join(octopusHomeRoot, "state", "runs", `${workspace.homeRunId}.json`);
    const record = JSON.parse(await fs.readFile(runPath, "utf8"));
    record.interface_state[provider] = { authoring_generation: 0, context_generation: 0,
      run_authoring_binding: { run: workspace.homeRunId } };
    await fs.writeFile(runPath, JSON.stringify(record));
    store.invalidate();
    assert.equal((await readStatuses()).chats[0].turn_open, false, "retired dispatch is history, not a turn in the fresh execution");
    record.interface_state[provider].authoring_generation = 1;
    await fs.writeFile(runPath, JSON.stringify(record));
    store.invalidate();
    assert.equal((await readStatuses()).chats[0].turn_open, false, "a control or catalog rearm cannot reopen the retired turn");
    const current = fixtureMovement(retired.seq + 1, workspace, "operator.message", workspace.homeInterfaceId, "run");
    current.invocation_metadata.runtime.checkpoint = "cp_home_input";
    await fs.appendFile(ledgerPath, `${JSON.stringify(current)}\n`);
    assert.equal((await readStatuses()).chats[0].turn_state, "waiting", "current input remains open before provider authoring");
    record.interface_state[provider].authoring_generation = 1;
    await fs.writeFile(runPath, JSON.stringify(record));
    store.invalidate();
    assert.equal((await readStatuses()).chats[0].turn_open, true, "normal rearming must not hide an unfinished turn");
    record.interface_state[provider].pending_handoff = { source_movement_seq: current.seq };
    record.checkpoints = record.checkpoints.filter((row) => row.id !== "cp_home_input");
    record.active_checkpoints = record.active_checkpoints.filter((id) => id !== "cp_home_input");
    await fs.writeFile(runPath, JSON.stringify(record));
    store.invalidate();
    assert.equal((await readStatuses()).chats[0].turn_open, true, "pending source retains a turn after its input checkpoint is replaced");
  } finally {
    store.close();
  }
}

async function verifyTaskArchive(temporary) {
  const fixtureHome = path.join(temporary, "task-archive-home");
  await createFixture(fixtureHome, path.join(fixtureHome, "registry"));
  const home = await fs.realpath(fixtureHome), registry = await fs.realpath(path.join(home, "registry"));
  const request = setupRequest("workspace_archive_12345678");
  const workerRoot = path.join(registry, "interface", request.home.workerInterfaceId);
  await fs.mkdir(workerRoot, { recursive: true });
  await fs.writeFile(path.join(workerRoot, "interface.json"), JSON.stringify({ id: request.home.workerInterfaceId, interface_type: "system", role: "worker", direction: "bidirectional", source_names: ["fixture"] }));
  await fs.writeFile(path.join(workerRoot, "policy.json"), JSON.stringify({ id: request.home.workerInterfaceId, role_artifacts: [], ceiling: { interfaces: [], tools: [], wek: [], mark_outcomes: [], hard_blocked_tools: [] } }));
  const workspace = await initializeWorkspace({ octopusHomeRoot: home, registryRoot: registry, request,
    planOperatorSurface: (input) => fixturePlan(home, registry, input) });
  const plan = await fixturePlan(home, registry, { eeInstancesRoot: path.join(home, "state", "ee_instances"),
    request: { ...request.home, eeId: workspace.projects[0].eeId, chatId: "archive_task_12345678", clientSurfaceRole: "task" } });
  for (const write of plan.writes) {
    await fs.mkdir(path.dirname(write.filePath), { recursive: true }); await fs.writeFile(write.filePath, write.content);
  }
  for (const [interfaceId, component] of [[plan.workerInterfaceId, "worker"], [plan.modelInterfaceId, "provider"]]) {
    await fs.writeFile(path.join(registry, "interface", interfaceId, "octopus_os.json"), JSON.stringify({
      schema_version: 1, workspace: "octopus_os", interface: interfaceId, owner_interface: plan.chatInterfaceId, component, surface_role: "task",
    }));
  }
  const protocolRoot = path.join(registry, "interface", "world_operator_chat");
  await fs.mkdir(protocolRoot, { recursive: true });
  await fs.copyFile(path.join(__dirname, "../../registry/interface/world_operator_chat/client.json"), path.join(protocolRoot, "client.json"));
  const runFile = path.join(home, "state", "runs", `${plan.runId}.json`), ledger = path.join(home, "ledger", "movements.jsonl");
  const original = JSON.parse(await fs.readFile(runFile, "utf8"));
  original.interface_state[plan.modelInterfaceId] = { authoring_generation: 1, pending_handoff: { terminal: true } };
  const intakeId = plan.requiredInterfaceIds.find((id) => id.startsWith("system_service_input_"));
  const doorId = plan.requiredInterfaceIds.find((id) => id.startsWith("system_input_writer_"));
  const toolId = plan.requiredToolIds.find((id) => id.startsWith("file.write.input_"));
  const intakePath = path.join(registry, "interface", intakeId, "interface.json");
  const providerPath = path.join(registry, "interface", plan.modelInterfaceId, "interface.json");
  const provider = JSON.parse(await fs.readFile(providerPath));
  provider.interface_driver_config.private_config.input_intake = { template_interface_id: "system_service_input", interface_id: intakeId };
  await fs.writeFile(providerPath, JSON.stringify(provider));
  const writer = { owner: intakeId, source: intakeId, door: doorId, tool: toolId, call_label: "tool.call", result_label: `${toolId}.result` };
  const declaration = { schema_version: 1, run: plan.runId, provider_interface: plan.modelInterfaceId, writer,
    writer_required_verified_artifacts: original.checkpoints[4].required_verified_artifacts,
    profile: { file_write_tool: toolId, file_write_label: writer.call_label, file_write_result_label: writer.result_label, result_timeout_ms: 30000 },
    provider_profile: { file_write_tool: "file.write.authoring", file_write_label: "tool.call", file_write_result_label: "file.write.authoring.result", result_timeout_ms: 30000 },
    input_label: "provider.peer.input", artifact_bindings: [] };
  const intake = { id: intakeId, interface_type: "system", role: "service", direction: "bidirectional", source_names: ["input"],
    interface_driver_ref: "checked_input_intake", interface_driver_config: { private_config: { input_intake: declaration } } };
  await fs.writeFile(intakePath, JSON.stringify(intake));
  original.interface_state[intakeId] = { input_intake: declaration,
    owned_checkpoint_ids: ["cp_intake_input", "cp_intake_call", "cp_intake_result"], frontier_checkpoint_ids: ["cp_intake_call", "cp_intake_result"] };
  const writeRun = (run) => fs.writeFile(runFile, `${JSON.stringify(run, null, 2)}\n`);
  await writeRun(original); await fs.mkdir(path.dirname(ledger)); await fs.writeFile(ledger, "");
  let store = createProjectionStore({ resolveRepoRoot: () => path.resolve(__dirname, "../.."), sourceRegistry: () => registry,
    octopusHome: () => home, projectEePolicies: () => fixturePolicyProjection(home) });
  const read = (method, params) => store.read({ contract_version: 1, method, params });
  const digest = async () => crypto.createHash("sha256").update(await fs.readFile(runFile)).digest("hex");
  let operations = 0;
  const archiveIdentity = { home, workspace: { rootEeId: workspace.rootEeId, homeRunId: workspace.homeRunId, homeInterfaceId: workspace.homeInterfaceId } };
  const hooks = {
    withTaskAuthoringLock: async (id, operation) => { assert.equal(id, plan.runId); return operation(); },
    readTaskStatus: async (id) => { store.invalidate(false); return (await read("projection.operator.chats.status", { run_ids: [id] })).chats[0]; },
    commitClientAuthoring: (mutation) => commitClientAuthoringTransaction({ octopusHomeRoot: home, registryRoot: registry, plan: {
      operationId: `task_archive_contract_${operations++}`, label: mutation.label,
      writes: mutation.writes.map((write) => ({ destination: write.file, expectedBeforeDigest: write.beforeDigest,
        content: `${JSON.stringify(write.next, null, 2)}\n`, tag: "task", publishBarrier: write.file === mutation.publishBarrierPath })), removals: [],
    } }),
  };
  const mutate = async (archived, overrides = {}) => {
    store.invalidate(false);
    const scope = await read("projection.operator.os.scope");
    assert.equal(scope.status, "ready", JSON.stringify(scope.errors));
    return mutateOctopusOsWorkspace(registry, home, scope,
      { action: "set_task_archived", ...archiveIdentity, runId: plan.runId, expectedDigest: await digest(), archived, ...overrides }, hooks);
  };
  try {
    await assert.rejects(mutate(true, { runId: workspace.homeRunId }), /not an archivable/);
    await assert.rejects(mutate(true, { home: `${home}-other` }), /does not match the current Home/);
    await assert.rejects(mutate(true, { expectedDigest: "0".repeat(64) }), /changed before/);
    const commitsBefore = operations;
    for (const [change, error] of [
      [(run) => { run.interface_state.system_service_foreign = {}; }, /outside its exact client topology/],
      [(run) => { run.interface_state[intakeId].input_intake.run = "foreign_run"; }, /exact run\/provider declaration/],
      [(run) => { run.interface_state[intakeId].input_intake.writer.door = plan.modelInterfaceId; }, /exact run\/provider declaration/],
      [(run) => { run.interface_state[intakeId].owned_checkpoint_ids.push("cp_home_input"); }, /exact ownership/],
      [(run) => { run.interface_state[plan.modelInterfaceId].owned_checkpoint_ids = ["cp_intake_input"]; }, /exact ownership/],
    ]) {
      const invalid = structuredClone(original); change(invalid); await writeRun(invalid);
      const before = await fs.readFile(runFile, "utf8");
      await assert.rejects(mutate(true), error);
      assert.equal(await fs.readFile(runFile, "utf8"), before); assert.equal(operations, commitsBefore);
    }
    await writeRun(original);
    const foreignDescriptor = structuredClone(intake); foreignDescriptor.interface_driver_config.private_config.input_intake.run = "foreign_run";
    await fs.writeFile(intakePath, JSON.stringify(foreignDescriptor));
    await assert.rejects(mutate(true), /exact run\/provider declaration/);
    await fs.writeFile(intakePath, JSON.stringify(intake));
    const policyPath = path.join(registry, "interface", plan.workerInterfaceId, "policy.json");
    const policyBefore = await fs.readFile(policyPath, "utf8"), policy = JSON.parse(policyBefore);
    policy.ceiling.interfaces = policy.ceiling.interfaces.filter((id) => id !== intakeId);
    await fs.writeFile(policyPath, JSON.stringify(policy)); await assert.rejects(mutate(true), /exact worker ceiling/);
    await fs.writeFile(policyPath, policyBefore);
    assert.equal(operations, commitsBefore);
    const pending = structuredClone(original); pending.interface_state[plan.modelInterfaceId].pending_handoff.terminal = false;
    await writeRun(pending); await assert.rejects(mutate(true), /is working/); await writeRun(original);
    const taskIdentity = { ...workspace, homeRunId: plan.runId, homeInterfaceId: plan.chatInterfaceId };
    await fs.appendFile(ledger, `${JSON.stringify(fixtureMovement(0, taskIdentity, "operator.message", plan.chatInterfaceId, "run"))}\n`);
    await assert.rejects(mutate(true), /open turn/);
    const reply = fixtureMovement(1, taskIdentity, "interface.artifact.output", plan.modelInterfaceId, "interface");
    reply.kind = "InterfaceOutbound";
    await fs.appendFile(ledger, `${JSON.stringify(reply)}\n`);
    store.invalidate(false);
    const messagesBefore = await read("projection.operator.chat.messages", { run_id: plan.runId });
    const archived = await mutate(true); assert.equal(archived.changed, true);
    const saved = JSON.parse(await fs.readFile(runFile, "utf8"));
    assert.deepEqual(saved.active_checkpoints, []);
    assert.deepEqual(saved.interface_state[plan.chatInterfaceId].client_archive.active_checkpoints, original.active_checkpoints);
    assert.deepEqual(saved.checkpoints, original.checkpoints); assert.equal(saved.status, original.status);
    store.close(); store = createProjectionStore({ resolveRepoRoot: () => path.resolve(__dirname, "../.."), sourceRegistry: () => registry, octopusHome: () => home, projectEePolicies: () => fixturePolicyProjection(home) });
    const archivedChat = (await read("projection.operator.chats.list")).chats.find((chat) => chat.run_id === plan.runId);
    assert.equal(archivedChat.archived, true); assert.equal(archivedChat.writable, false); assert.equal(archivedChat.message_count, 2);
    assert.equal(archivedChat.provider_bound_interface_id, plan.modelInterfaceId);
    assert.equal((await read("projection.operator.chats.status", { run_ids: [plan.runId] })).chats[0].archived, true);
    assert.deepEqual(await read("projection.operator.chat.messages", { run_id: plan.runId }), messagesBefore);
    assert.equal((await mutate(true)).changed, false); assert.equal(operations, 1);
    const scope = await read("projection.operator.os.scope");
    await assert.rejects(mutateOctopusOsWorkspace(registry, home, scope,
      { action: "delete_task", runId: plan.runId, expectedDigest: await digest() }, hooks), /restore it before deleting/);
    await assert.rejects(mutateOctopusOsWorkspace(registry, home, scope,
      { action: "move_task", runId: plan.runId, targetEeId: workspace.projects[1].eeId, expectedDigest: await digest() }, hooks), /restore it before moving/);
    const tampered = structuredClone(saved); tampered.checkpoints[0].epoch += 1; await writeRun(tampered);
    scope.client_runs.find((row) => row.run_id === plan.runId).record_digest = await digest();
    await assert.rejects(mutateOctopusOsWorkspace(registry, home, scope,
      { action: "set_task_archived", ...archiveIdentity, runId: plan.runId, expectedDigest: await digest(), archived: false }, hooks), /topology changed/);
    await writeRun(saved);
    const reopened = structuredClone(saved); reopened.active_checkpoints = [...original.active_checkpoints]; await writeRun(reopened);
    scope.client_runs.find((row) => row.run_id === plan.runId).record_digest = await digest();
    await assert.rejects(mutateOctopusOsWorkspace(registry, home, scope,
      { action: "set_task_archived", ...archiveIdentity, runId: plan.runId, expectedDigest: await digest(), archived: false }, hooks), /still admits artifacts/);
    await writeRun(saved);
    assert.equal((await mutate(false)).changed, true);
    assert.deepEqual(JSON.parse(await fs.readFile(runFile, "utf8")), original);
    store.invalidate(false);
    const restoredChat = (await read("projection.operator.chats.list")).chats.find((chat) => chat.run_id === plan.runId);
    assert.equal(restoredChat.archived, false); assert.equal(restoredChat.writable, true);
    assert.equal(operations, 2);
    console.log("task archive: checked writes, admission frontier, retained history, explicit restore and drift refusals passed");
  } finally { store.close(); }
}

function createFixtureLedgerIndex(indexPath) {
  let DatabaseSync;
  try {
    ({ DatabaseSync } = require("node:sqlite"));
  } catch (error) {
    if (error?.code === "ERR_UNKNOWN_BUILTIN_MODULE") return false;
    throw error;
  }
  const database = new DatabaseSync(indexPath);
  try {
    database.exec(`
      CREATE TABLE index_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE movements(
        seq INTEGER PRIMARY KEY,
        lamport INTEGER NOT NULL,
        recorded_at_unix_ms INTEGER NOT NULL,
        trace TEXT NOT NULL,
        kind TEXT NOT NULL,
        mark TEXT,
        from_kind TEXT NOT NULL,
        from_id TEXT NOT NULL,
        to_kind TEXT NOT NULL,
        to_id TEXT NOT NULL,
        artifact_id TEXT,
        artifact_label TEXT,
        artifact_content TEXT,
        runtime_run TEXT,
        runtime_checkpoint TEXT,
        runtime_interface TEXT,
        start_offset INTEGER,
        end_offset INTEGER
      );
      INSERT INTO index_meta(key, value) VALUES
        ('schema_version', '4'),
        ('watermark', '0'),
        ('durable_end_offset', '0');
    `);
  } finally {
    database.close();
  }
  return true;
}

async function indexFixtureMovements(ledgerRoot, movements) {
  const { DatabaseSync } = require("node:sqlite");
  const database = new DatabaseSync(path.join(ledgerRoot, "ledger-index.sqlite"));
  try {
    const insert = database.prepare(`INSERT INTO movements(
      seq, lamport, recorded_at_unix_ms, trace, kind,
      from_kind, from_id, to_kind, to_id,
      artifact_id, artifact_label, artifact_content,
      runtime_run, runtime_checkpoint, runtime_interface
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`);
    for (const movement of movements) {
      const [fromKind, fromId] = Object.entries(movement.from)[0];
      const [toKind, toId] = Object.entries(movement.to)[0];
      insert.run(
        movement.seq,
        movement.lamport,
        movement.recorded_at_unix_ms,
        movement.trace,
        movement.kind,
        fromKind.toLowerCase(),
        fromId,
        toKind.toLowerCase(),
        toId,
        movement.artifact.id,
        movement.invocation_metadata.checkpoint_admission.artifact_label,
        Buffer.from(movement.artifact.content).toString("hex"),
        movement.invocation_metadata.runtime.run,
        movement.invocation_metadata.runtime.checkpoint,
        movement.invocation_metadata.runtime.interface,
      );
    }
    const ledgerSize = (await fs.stat(path.join(ledgerRoot, "movements.jsonl"))).size;
    database.prepare("UPDATE index_meta SET value = ? WHERE key = 'watermark'").run(String(movements.length));
    database.prepare("UPDATE index_meta SET value = ? WHERE key = 'durable_end_offset'").run(String(ledgerSize));
  } finally {
    database.close();
  }
}

async function fixturePolicyProjection(octopusHomeRoot) {
  const policy = {
    allowed_interfaces: [],
    allowed_tools: [],
    allowed_worker_environment_kinds: [],
    allowed_root_slots: [],
    allowed_artifact_verbs: [],
    hard_blocked_tools: [],
  };
  const files = (await fs.readdir(path.join(octopusHomeRoot, "state", "ee_instances")))
    .filter((name) => name.endsWith(".json"))
    .sort();
  const environments = [];
  for (const file of files) {
    const record = JSON.parse(await fs.readFile(path.join(octopusHomeRoot, "state", "ee_instances", file), "utf8"));
    environments.push({
      ee_id: record.ee_id,
      effective_policy: policy,
      policy_ceiling: policy,
      workers: [],
    });
  }
  return { environments, issues: [] };
}

function fixtureMovement(seq, workspace, label, fromInterface, toKind) {
  const run = workspace.homeRunId;
  return {
    seq,
    lamport: seq + 1,
    recorded_at_unix_ms: 1_780_000_000_000 + seq,
    trace: `trace:${run}:status:${seq}`,
    kind: "InterfaceArrival",
    from: { Interface: fromInterface },
    to: { Interface: toKind === "run" ? `system_worker_status_${run.replace("run_operator_chat_", "")}` : workspace.homeInterfaceId },
    artifact: { id: `${label}:${String(seq).padStart(64, "0")}`, content: Array(32).fill(seq) },
    invocation_metadata: {
      checkpoint_admission: { artifact_label: label, complete: true, required_artifact_labels: [] },
      runtime: {
        checkpoint: `cp_status_${seq}`,
        ee: workspace.rootEeId,
        epoch: 0,
        interface: fromInterface,
        run,
        worker: `system_worker_status_${run.replace("run_operator_chat_", "")}`,
      },
    },
  };
}

async function verifyTerminalBootstrapBoundary() {
  const [mainSource, operatorClientSource] = await Promise.all([
    fs.readFile(path.join(__dirname, "../electron/main.cjs"), "utf8"),
    fs.readFile(path.join(__dirname, "../../registry/interface/world_operator_chat/client.cjs"), "utf8"),
  ]);
  const runtimeInitialization = mainSource.match(
    /async function initializeRuntimeHost\(\)[\s\S]*?(?=\nfunction getTerminalHost)/,
  )?.[0] ?? "";
  assert.doesNotMatch(
    `${runtimeInitialization}\n${operatorClientSource}`,
    /providerProjectionPreflight|synchronizeStoppedOperatorProviderProjections|loadSourceOperatorChatClient|synchronizeOperatorChatProviderProjectionContracts|planOperatorChatProviderProjectionSynchronization|runtime_stopped/,
  );
}

async function crashChild(octopusHomeRoot, registryRoot) {
  await initializeWorkspace({
    octopusHomeRoot,
    registryRoot,
    request: setupRequest("workspace_crash_12345678"),
    planOperatorSurface: (input) => fixturePlan(octopusHomeRoot, registryRoot, input),
    onProgress: (event) => {
      if (event.phase === "applying") process.exit(73);
    },
  });
  process.exit(74);
}

async function projectCrashChild(
  mode,
  octopusHomeRoot,
  registryRoot,
  rootEeId,
  homeRunId,
  homeInterfaceId,
  targetEeId,
) {
  const workspace = { rootEeId, homeRunId, homeInterfaceId };
  const scope = await fixtureProjectScope(octopusHomeRoot, registryRoot, workspace);
  if (mode === "create") {
    const operationId = "project_crash_create_12345678";
    armProjectCrash(
      octopusHomeRoot,
      operationId,
      path.join(registryRoot, "interface", homeInterfaceId, "octopus_os.json"),
    );
    await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope,
      planOperatorSurface: (input) => fixturePlan(octopusHomeRoot, registryRoot, input),
      request: {
        action: "create_project",
        operationId,
        workspace: fixtureWorkspaceIdentity(scope),
        parentEeId: rootEeId,
        expectedParentDigest: scope.environments.find((environment) => environment.eeId === rootEeId).recordDigest,
        templateName: "project",
        name: "Crash Recovery",
        goal: "Crash Recovery",
        presentation: { icon: "bolt", color: "amber" },
      },
    });
  } else if (mode === "delete") {
    const operationId = "project_crash_delete_12345678";
    const target = scope.environments.find((environment) => environment.eeId === targetEeId);
    assert.ok(target);
    armProjectCrash(
      octopusHomeRoot,
      operationId,
      path.join(octopusHomeRoot, "state", "ee_instances", `${targetEeId}.json`),
    );
    await mutateWorkspaceProject({
      octopusHomeRoot,
      registryRoot,
      scope,
      request: {
        action: "delete_project",
        operationId,
        workspace: fixtureWorkspaceIdentity(scope),
        eeId: targetEeId,
        expectedProjectDigest: target.recordDigest,
      },
    });
  } else {
    throw new Error(`unknown Project crash mode '${mode}'`);
  }
  process.exit(74);
}

function armProjectCrash(octopusHomeRoot, operationId, publishBarrierPath) {
  const journalPath = path.join(
    octopusHomeRoot,
    "client-state",
    "workspace",
    "transactions",
    operationId,
    "journal.json",
  );
  const originalRename = fs.rename;
  const originalOpen = fs.open;
  let armed = false;
  fs.rename = async (source, destination) => {
    if (destination === journalPath) {
      const journal = JSON.parse(await fs.readFile(source, "utf8"));
      armed = journal.schema === "octopus.workspace_project_transaction"
        && journal.phase === "committing"
        && journal.writes[0]?.state === "applied"
        && journal.writes.some((writeRow) => writeRow.publish_barrier && writeRow.state === "staged");
    }
    return originalRename(source, destination);
  };
  fs.open = async (file, ...args) => {
    if (armed && file === publishBarrierPath) process.exit(73);
    return originalOpen(file, ...args);
  };
}

async function verifyProjectionReadinessBarrier(temporary) {
  const fixtureHome = path.join(temporary, "projection-readiness-home");
  const registryRoot = path.join(fixtureHome, "registry");
  await createFixture(fixtureHome, registryRoot);
  const octopusHomeRoot = await fs.realpath(fixtureHome);
  const runsRoot = path.join(octopusHomeRoot, "state", "runs");
  await fs.mkdir(runsRoot, { recursive: true });
  await fs.writeFile(path.join(octopusHomeRoot, "host.json"), JSON.stringify({
    host_manifest_version: 1,
    root_refs: { run_records_root: runsRoot },
  }));
  let mode = "pass";
  let releaseRecovery;
  let beforeReadCalls = 0;
  let policyProjectionCalls = 0;
  const recovery = new Promise((resolve) => { releaseRecovery = resolve; });
  let store;
  store = createProjectionStore({
    resolveRepoRoot: () => path.resolve(__dirname, "../.."),
    sourceRegistry: () => registryRoot,
    octopusHome: () => octopusHomeRoot,
    projectEePolicies: async () => {
      policyProjectionCalls += 1;
      return { environments: [], issues: [] };
    },
    beforeRead: async () => {
      beforeReadCalls += 1;
      if (mode === "defer") await recovery;
      if (mode === "reject") throw new Error("workspace recovery failed closed");
      if (mode === "defer" || mode === "retry") store.invalidate();
    },
  });
  const request = { contract_version: 1, method: "projection.ee_instances.list", params: { include_ledger: false } };
  try {
    const initial = await store.read(request);
    assert.equal(initial.ee_instances.some((entry) => entry.ee_id === "ee_after_recovery"), false);
    assert.equal(policyProjectionCalls, 1);
    store.invalidate(false);
    await store.read(request);
    assert.equal(policyProjectionCalls, 1);
    mode = "defer";
    let settled = false;
    const deferred = store.read(request);
    deferred.then(() => { settled = true; }, () => { settled = true; });
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(settled, false);
    await fs.writeFile(path.join(octopusHomeRoot, "state", "ee_instances", "ee_after_recovery.json"), `${JSON.stringify({
      ee_id: "ee_after_recovery",
      template_name: "project",
      parent_ee_id: "ee_existence",
      goal: "Recovered before projection",
      config: {},
    }, null, 2)}\n`);
    releaseRecovery();
    const recovered = await deferred;
    assert.equal(recovered.ee_instances.some((entry) => entry.ee_id === "ee_after_recovery"), true);
    mode = "reject";
    await assert.rejects(store.read(request), /workspace recovery failed closed/);
    await fs.writeFile(path.join(octopusHomeRoot, "state", "ee_instances", "ee_after_retry.json"), `${JSON.stringify({
      ee_id: "ee_after_retry",
      template_name: "project",
      parent_ee_id: "ee_existence",
      goal: "Recovered on retry",
      config: {},
    }, null, 2)}\n`);
    mode = "retry";
    const retried = await store.read(request);
    assert.equal(retried.ee_instances.some((entry) => entry.ee_id === "ee_after_retry"), true);
    assert.equal(beforeReadCalls, 5);
  } finally {
    store.close();
  }
}

async function verifyCapabilityPreferenceSetup() {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-capability-preferences-")));
  const browser = require("../../registry/worker_environment_kind/browser_session/packages/octopus-browser-session-env/native/setup-host.cjs");
  const desktop = require("../../registry/worker_environment_kind/desktop_session/packages/octopus-system-gui-env/native/setup-preferences.cjs");
  try {
    for (const fail of [false, true]) {
      const home = path.join(root, fail ? "rollback" : "complete"), registry = path.join(home, "registry");
      await createFixture(home, registry);
      const original = `${JSON.stringify({ host_manifest_version:1,root_refs:{ retained_root:root } })}\n`;
      await fs.writeFile(path.join(home,"host.json"),original);
      const request = setupRequest(fail ? "prefs_rollback_12345678" : "prefs_complete_12345678"); request.projects = [];
      const rollback = [];
      const options = { octopusHomeRoot:home, registryRoot:registry, request,
        planOperatorSurface:async input => {
          const plan = await fixturePlan(home,registry,input);
          for (const family of ["browser", "gui"]) {
            const id = `system_${family}_${input.request.chatId}`, dir=path.join(registry,"interface",id);
            plan.requiredInterfaceIds.push(id);
            plan.writes.push(write(path.join(dir,"interface.json"),JSON.stringify({id,...(family==='browser'?{interface_driver_config:{private_config:{browser_room:{transport:{wire:'browser.native.v1',host_root_slot:'browser_native_host_root'}}}}}:{})}),`interface:${id}`));
            if (family==='gui') plan.writes.push(write(path.join(dir,"desktop.json"),JSON.stringify({host:{application_preferences:{root_slot:'desktop_app_preferences_root'}}}),`desktop:${id}`));
            const runWrite = plan.writes.find(row => row.tag.startsWith('run:')), run = JSON.parse(runWrite.content);
            run.checkpoints[0].outbound_interface_bindings.push({interface:id,config:{artifact_policy:{mode:'all'}}});
            runWrite.content = JSON.stringify(run);
            const policyWrite = plan.writes.find(row => row.tag.startsWith('policy:')), policy = JSON.parse(policyWrite.content);
            policy.ceiling.interfaces.push(id); policyWrite.content = JSON.stringify(policy);
          }
          plan.requiredInterfaceIds.sort();return plan;
        },
        planBrowserNativeHost:async input=>{const plan=await browser.planBrowserNativeHost(input);return {...plan,rollback:async()=>{rollback.push('browser');await plan.rollback();}};},
        planDesktopApplicationPreferences:async input=>{
          assert.equal(input.plannedWrites.filter(row=>row.destination===path.join(home,'host.json')).length,1);
          const nullPreferences={destination:path.join(registry,'interface','system_gui_disabled','desktop.json'),content:JSON.stringify({host:{application_preferences:null}})};
          const plan=await desktop.planDesktopApplicationPreferences({...input,plannedWrites:[nullPreferences,...input.plannedWrites]});
          const host=JSON.parse(plan.writes[0].content);assert.equal(host.root_refs.browser_native_host_root,path.join(home,'client/browser-native-host'));
          assert.equal(host.root_refs.desktop_app_preferences_root,path.join(home,'client/desktop-app-preferences'));assert.equal(host.root_refs.retained_root,root);
          return {...plan,writes:plan.writes.map(row=>({...row,expectedBeforeDigest:fail?'0'.repeat(64):row.expectedBeforeDigest})),rollback:async()=>{rollback.push('desktop');await plan.rollback();}};
        },
      };
      if(fail){
        await assert.rejects(initializeWorkspace(options),/invalid merged host manifest plan/);
        assert.deepEqual(rollback,['desktop','browser']);assert.equal(await fs.readFile(path.join(home,'host.json'),'utf8'),original);
        for (const name of ['browser-native-host','desktop-app-preferences']) await assert.rejects(fs.access(path.join(home,'client',name)), {code:'ENOENT'});
        assert.equal((await fs.stat(path.join(home,'client','agents'))).isDirectory(),true,'shared Agent setup is preserved');
      }else{
        const outcome=await initializeWorkspace(options);assert.equal((await readWorkspaceSetupState(options)).status,'configured');
        const receipt=JSON.parse(await fs.readFile(path.join(home,'client-state/workspace/receipts',`${request.operationId}.json`),'utf8'));
        assert.equal(receipt.writes.filter(row=>row.path===path.join(home,'host.json')).length,1);
        assert.equal(JSON.parse(await fs.readFile(path.join(home,'host.json'),'utf8')).root_refs.retained_root,root);assert.deepEqual(rollback,[]);
        assert.ok(outcome.homeRunId);
      }
    }
    console.log('Capability preference setup: actual two helpers merge one host write, preserve original roots, and roll back both lifetimes before publication');
  } finally {await fs.rm(root,{recursive:true,force:true});}
}

async function verifyRetiredConsentSetup() {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-consent-setup-test-"));
  let store;
  try {
    const home = path.join(await fs.realpath(temporary), "home");
    const registry = path.join(home, "registry");
    await createFixture(home, registry);
    const request = setupRequest("consent_setup_12345678");
    request.projects = [];
    let plannerCalls = 0;
    const options = { octopusHomeRoot: home, registryRoot: registry, request,
      planOperatorSurface: async (input) => { plannerCalls += 1; return fixturePlan(home, registry, input); } };
    await assert.rejects(initializeWorkspace({ ...options, request: { ...request,
      applicationConsentAuthority: { packId: "codex", expectedPackManifestDigest: "1".repeat(64) } } }), /unknown fields: applicationConsentAuthority/);
    assert.equal(plannerCalls, 0);
    await assert.rejects(fs.access(path.join(home, "state", "runs")), { code: "ENOENT" });
    const outcome = await initializeWorkspace(options);
    assert.equal(plannerCalls, 1);
    const descriptorFile = path.join(registry, "interface", outcome.homeInterfaceId, "interface.json");
    const descriptor = JSON.parse(await fs.readFile(descriptorFile, "utf8"));
    assert.equal(descriptor.interface_driver_config?.private_config?.application_consent_authority, undefined);
    assert.equal((await readWorkspaceSetupState(options)).status, "configured");
    await fs.mkdir(path.join(home, "ledger")); await fs.writeFile(path.join(home, "ledger", "movements.jsonl"), "");
    store = createProjectionStore({ resolveRepoRoot: () => path.resolve(__dirname, "../.."), sourceRegistry: () => registry, octopusHome: () => home, projectEePolicies: () => fixturePolicyProjection(home) });
    const readScope = () => store.read({ contract_version: 1, method: "projection.operator.os.scope" });
    assert.equal((await readScope()).status, "ready");
    const rootFile = path.join(home, "state", "ee_instances", `${outcome.rootEeId}.json`);
    const child = { ...JSON.parse(await fs.readFile(rootFile, "utf8")), ee_id: "ee_model_created", parent_ee_id: outcome.rootEeId, goal: "Model-authored goal" };
    await fs.writeFile(path.join(home, "state", "ee_instances", `${child.ee_id}.json`), JSON.stringify(child));
    store.invalidate();
    const expanded = await readScope();
    assert.equal(expanded.status, "ready", expanded.errors.join("; "));
    assert.ok(expanded.projects.some((project) => project.ee_id === child.ee_id));
    assert.ok(!expanded.task_authoring_profiles.some((profile) => profile.ee_id === child.ee_id));
    assert.ok(!expanded.project_presentations.some((project) => project.ee_id === child.ee_id));
    const selected = { ...descriptor, interface_driver_config: { ...descriptor.interface_driver_config,
      private_config: { ...descriptor.interface_driver_config?.private_config,
        application_consent_authority: { id: "artifact-retired-consent", content: Array(32).fill(1) } } } };
    const selectedBytes = `${JSON.stringify(selected, null, 2)}\n`;
    await fs.writeFile(descriptorFile, selectedBytes);
    store.invalidate();
    const invalid = await readScope();
    assert.equal(invalid.status, "invalid");
    assert.ok(invalid.errors.some((error) => error.includes("application_consent_authority selection is retired")));
    assert.equal(invalid.application_consent_authority, undefined);
    const state = await readWorkspaceSetupState(options);
    assert.equal(state.status, "degraded");
    assert.ok(state.errors.some((error) => error.includes("application_consent_authority selection is retired")));
    assert.equal(await fs.readFile(descriptorFile, "utf8"), selectedBytes);
    const descriptorBytes = `${JSON.stringify(descriptor, null, 2)}\n`;
    await fs.writeFile(descriptorFile, descriptorBytes);
    const receiptFile = path.join(home, "client-state", "workspace", "receipts", `${request.operationId}.json`);
    const receipt = JSON.parse(await fs.readFile(receiptFile, "utf8"));
    const transaction = path.join(home, "client-state", "workspace", "transactions", request.operationId);
    await fs.mkdir(path.join(transaction, "after"), { recursive: true });
    await fs.mkdir(path.join(transaction, "before"));
    const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
    const journalWrites = [];
    for (const write of receipt.writes) {
      const bytes = await fs.readFile(write.path);
      const selectedHome = write.path === descriptorFile;
      journalWrites.push({ destination: write.path, expected_before_digest: hash(bytes), before_digest: hash(bytes),
        after_digest: selectedHome ? hash(selectedBytes) : hash(bytes), after_file: selectedHome ? "after/home.bin" : "",
        before_file: selectedHome ? "before/home.bin" : "", tag: write.tag, publish_barrier: write.publish_barrier,
        state: selectedHome ? "staged" : "unchanged" });
    }
    await fs.writeFile(path.join(transaction, "after", "home.bin"), selectedBytes);
    await fs.writeFile(path.join(transaction, "before", "home.bin"), descriptorBytes);
    const journal = { schema: "octopus.workspace_setup_transaction", schema_version: 1, operation_id: request.operationId,
      request_digest: receipt.request_digest, phase: "staged", writes: journalWrites, receipt: { ...receipt,
        writes: journalWrites.map((write) => ({ path: write.destination, digest: write.after_digest, tag: write.tag, publish_barrier: write.publish_barrier })) } };
    const journalBytes = `${JSON.stringify(journal, null, 2)}\n`;
    await fs.writeFile(path.join(transaction, "journal.json"), journalBytes);
    await assert.rejects(recoverWorkspaceTransactions(options), /application_consent_authority selection is retired/);
    assert.equal(await fs.readFile(descriptorFile, "utf8"), descriptorBytes);
    assert.equal(await fs.readFile(path.join(transaction, "journal.json"), "utf8"), journalBytes);
    console.log("retired Home consent selection refuses setup, projection, and pending replay without conversion");
  } finally { store?.close(); await fs.rm(temporary, { recursive: true, force: true }); }
}

async function verifyFreshAgentRecipe(temporary) {
  temporary = await fs.realpath(temporary);
  const { planClientAgentTemplate, readClientAgentCatalog } = require("../electron/clientAgentCatalog.cjs");
  const sourceRoot = path.resolve(__dirname, "../../registry/interface/system_worker_computer_use");
  const sourceInterface = JSON.parse(await fs.readFile(path.join(sourceRoot, "interface.json")));
  const sourcePolicy = JSON.parse(await fs.readFile(path.join(sourceRoot, "policy.json")));
  for (const admitted of [true, false]) {
    const home = path.join(temporary, admitted ? "agent-recipe" : "agent-refused");
    const registry = path.join(home, "registry");
    await createFixture(home, registry);
    const templatePath = path.join(registry, "ee_template", "project", "ee_template.json");
    const template = JSON.parse(await fs.readFile(templatePath));
    template.allowed_interfaces = admitted ? [sourceInterface.id] : [];
    await fs.writeFile(templatePath, `${JSON.stringify(template, null, 2)}\n`);
    const request = setupRequest(admitted ? "recipe_admitted_001" : "recipe_refused_001");
    request.agentPresets = [{ packId: "fixture", expectedPackManifestDigest: "a".repeat(64), presetId: "computer-use", projectKeys: ["work"], providerInterfaceId: request.home.providerInterfaceId, providerModel: request.home.providerModel, reasoningEffort: "high" }];
    const options = {
      octopusHomeRoot: home, registryRoot: registry, request,
      planOperatorSurface: (input) => fixturePlan(home, registry, input),
      planAgentPreset: async ({ choice, projectEeId, operationId }) => ({
        ...planClientAgentTemplate({ operationId, projectEeId, sourceInterface, sourcePolicy, name: "Computer Use", icon: "goggles",
          sourceProfile: { ee_id: projectEeId, worker_choice_interface: sourceInterface.id, door_host_slots: {}, context_artifact_bindings: sourcePolicy.role_artifacts.map((id) => ({ id })), selected_worker_environment_kinds: ["desktop_session"] },
          settings: { schema_version: 1, worker_interface: sourceInterface.id, project_ee_id: projectEeId, provider_interface_id: choice.providerInterfaceId, reasoning_effort: choice.reasoningEffort, application_ids: [], worker_environment_kinds: ["desktop_session"] },
        }), verify: async () => {},
      }),
    };
    if (!admitted) {
      await assert.rejects(initializeWorkspace(options), /does not admit exact source worker/);
      assert.deepEqual(await fs.readdir(path.join(home, "state", "runs")), []);
      assert.equal((await readClientAgentCatalog(home)).exists, false);
      continue;
    }
    const result = await initializeWorkspace(options);
    const catalog = await readClientAgentCatalog(home);
    assert.equal(catalog.document.agents.length, 1);
    const agent = catalog.document.agents[0];
    const profile = JSON.parse(await fs.readFile(path.join(registry, "interface", result.homeInterfaceId, "octopus_os.json"))).task_authoring_profiles.find((row) => row.worker_choice_interface === agent.worker_interface);
    assert.deepEqual(profile.context_artifact_bindings.map((row) => row.id), sourcePolicy.role_artifacts);
    assert.deepEqual(JSON.parse(await fs.readFile(path.join(registry, "interface", agent.worker_interface, "policy.json"))), { ...sourcePolicy, id: agent.worker_interface });
    assert.deepEqual(JSON.parse(await fs.readFile(templatePath)).allowed_interfaces, [sourceInterface.id, agent.worker_interface].sort());
    assert.equal(catalog.document.project_sources.find((row) => row.project_ee_id === agent.project_ee_id).source_worker_interface, request.home.workerInterfaceId);
    assert.equal((await fs.readdir(path.join(home, "state", "runs"))).length, 1);
  }
}

async function verifyExistingRootAttachment(temporary) {
  const fixture = path.join(temporary, "existing-root");
  await createFixture(fixture, path.join(fixture, "registry"));
  const octopusHomeRoot = await fs.realpath(fixture);
  const registryRoot = path.join(octopusHomeRoot, "registry");
  const rootPath = path.join(octopusHomeRoot, "state/ee_instances/ee_existence.json");
  const root = JSON.parse(await fs.readFile(rootPath, "utf8"));
  Object.assign(root, { goal: "Preserve the authored goal", config: { selected: ["keep"] }, policy_narrowing: { allowed_tools: ["file.read"] } });
  const before = `${JSON.stringify(root, null, 2)}\n`;
  await fs.writeFile(rootPath, before);
  const request = setupRequest("existing_root_12345678");
  request.root = { key: "root", existingEeId: root.ee_id, expectedEeDigest: crypto.createHash("sha256").update(before).digest("hex"), name: "Attached" };
  request.projects = [];
  request.home.workerInterfaceId = "system_worker_experiment";
  request.home.selectedWorkerEnvironmentKinds = ["git_worktree", "process_session"];
  const planOperatorSurface = async (input) => {
    assert.equal(await fs.readFile(path.join(input.eeInstancesRoot, "ee_existence.json"), "utf8"), before);
    assert.equal(input.request.eeId, root.ee_id);
    assert.equal(input.request.workerInterfaceId, request.home.workerInterfaceId);
    assert.deepEqual(input.request.selectedWorkerEnvironmentKinds, request.home.selectedWorkerEnvironmentKinds);
    return fixturePlan(octopusHomeRoot, registryRoot, input);
  };
  const options = { octopusHomeRoot, registryRoot, request, planOperatorSurface };
  await assert.rejects(initializeWorkspace({ ...options, request: { ...request, root: { ...request.root, expectedEeDigest: "0".repeat(64) } } }), /changed before planning/);
  await assert.rejects(initializeWorkspace({ ...options, request: { ...request, root: { ...request.root, goal: "Rewrite" } } }), /unknown fields/);
  const foreign = JSON.stringify({ ...root, ee_id: "ee_foreign" });
  await fs.writeFile(rootPath, foreign);
  await assert.rejects(initializeWorkspace({ ...options, request: { ...request, root: { ...request.root, expectedEeDigest: crypto.createHash("sha256").update(foreign).digest("hex") } } }), /foreign identity/);
  await fs.writeFile(rootPath, before);
  await assert.rejects(initializeWorkspace({ ...options, planOperatorSurface: async (input) => {
    const plan = await planOperatorSurface(input);
    const write = plan.writes.find((row) => row.tag.startsWith("run:"));
    write.content = JSON.stringify({ ...JSON.parse(write.content), ee: "ee_foreign" });
    return plan;
  } }), /changed its run, EE/);
  await assert.rejects(initializeWorkspace({ ...options, planOperatorSurface: async (input) => {
    const plan = await planOperatorSurface(input);
    await fs.writeFile(rootPath, `${before}\n`);
    return plan;
  } }), /changed during initialization/);
  await fs.writeFile(rootPath, before);
  let drifted = false;
  await assert.rejects(initializeWorkspace({ ...options, onProgress: (event) => {
    if (event.phase === "applying" && !drifted) {
      require("node:fs").writeFileSync(rootPath, `${before}\n`);
      drifted = true;
    }
  } }), /changed during initialization/);
  assert.equal(drifted, true);
  assert.equal(await fs.readFile(rootPath, "utf8"), `${before}\n`);
  assert.deepEqual(await fs.readdir(path.join(octopusHomeRoot, "state/runs")), []);
  assert.equal((await readWorkspaceSetupState(options)).status, "not_configured");
  await fs.writeFile(rootPath, before);
  const outcome = await initializeWorkspace(options);
  assert.equal(outcome.rootEeId, root.ee_id);
  assert.deepEqual(await fs.readdir(path.dirname(rootPath)), ["ee_existence.json"]);
  assert.equal(await fs.readFile(rootPath, "utf8"), before);
  const state = await readWorkspaceSetupState(options);
  assert.equal(state.status, "configured");
  const artifactWrite = state.receipt.writes.find((write) => write.tag.startsWith("artifact:"));
  const artifactBytes = await fs.readFile(artifactWrite.path);
  assert.equal(crypto.createHash("sha256").update(artifactBytes).digest("hex"), artifactWrite.digest);
  assert.equal(JSON.parse(artifactBytes).text, "Exact fixture Home context");
  assert.equal(state.receipt.existing_root.digest, request.root.expectedEeDigest);
  assert.equal(state.receipt.writes.some((write) => write.path === rootPath), false);
  const runPath = path.join(octopusHomeRoot, "state/runs", `${outcome.homeRunId}.json`);
  const run = JSON.parse(await fs.readFile(runPath, "utf8"));
  run.status = "idle";
  run.checkpoints[0].epoch += 1;
  await fs.writeFile(runPath, JSON.stringify(run));
  assert.equal((await readWorkspaceSetupState(options)).status, "configured");
  assert.equal((await initializeWorkspace(options)).receiptDigest, outcome.receiptDigest);
  const receiptPath = path.join(octopusHomeRoot, "client-state/workspace/receipts", `${request.operationId}.json`);
  const receipt = state.receipt;
  receipt.existing_root.path = path.join(temporary, "foreign-home/state/ee_instances/ee_existence.json");
  await fs.writeFile(receiptPath, JSON.stringify(receipt));
  const refused = await readWorkspaceSetupState(options);
  assert.equal(refused.status, "degraded");
  assert.match(refused.errors.join("\n"), /another Home/);
}

async function verifyRootFirstSetup(temporary) {
  const filename = path.resolve(__dirname, "../src/onboarding/workspaceSetup.ts"), ts = require("typescript"), compiled = { exports: {} };
  const code = ts.transpileModule(await fs.readFile(filename, "utf8"), { fileName: filename,
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  Function("module", "exports", "require", code)(compiled, compiled.exports, require("node:module").createRequire(filename));
  const { createWorkspaceSetupDraft, validateWorkspaceSetupDraft, compileWorkspaceInitializeRequest, workspaceInitializeRequestDigest } = compiled.exports;
  let sequence = 0;
  const draft = createWorkspaceSetupDraft("founder", () => `root_first_${++sequence}`);
  assert.equal(Object.hasOwn(draft, "providerModel"), false);
  assert.throws(() => validateWorkspaceSetupDraft({ ...draft, providerModel: 42 }), /Selected model must be text/);
  const missingGoal = { ...draft }; delete missingGoal.spaceGoal;
  assert.throws(() => validateWorkspaceSetupDraft(missingGoal), /missing fields: spaceGoal/);
  Object.assign(draft, { spaceName: "Chosen space", spaceGoal: "Chosen root goal", providerModel: "fixture-model", reasoningEffort: "high" });
  const recipe = setupRequest("recipe_root_12345678").home;
  const request = compileWorkspaceInitializeRequest(draft, { provider_interface_id: recipe.providerInterfaceId, worker_interface_id: recipe.workerInterfaceId,
    context_artifact_bindings: recipe.contextArtifactBindings, selected_worker_environment_kinds: recipe.selectedWorkerEnvironmentKinds, project_defaults: [] });
  assert.deepEqual(request.root, { key: "root", templateName: "existence", name: draft.spaceName, goal: draft.spaceGoal });
  const home = path.join(await fs.realpath(temporary), "root-first"), registry = path.join(home, "registry");
  await createFixture(home, registry);
  await fs.rm(path.join(registry, "ee_template", "operator_space"), { recursive: true });
  const rootPath = path.join(home, "state", "ee_instances", "ee_existence.json"), templatePath = path.join(registry, "ee_template", "existence", "ee_template.json");
  const root = JSON.parse(await fs.readFile(rootPath));
  Object.assign(root, { config: { selected: ["preserved"] }, policy_narrowing: { allowed_worker_environment_kinds: ["git_worktree"] } });
  const before = `${JSON.stringify(root, null, 2)}\n`, template = await fs.readFile(templatePath);
  await fs.writeFile(rootPath, before);
  const options = { octopusHomeRoot: home, registryRoot: registry, request, planOperatorSurface: (input) => fixturePlan(home, registry, input) };
  await assert.rejects(initializeWorkspace({ ...options, request: { ...request, root: { ...request.root, templateName: "operator_space" } } }), /actual bootstrap-root/);
  await fs.writeFile(templatePath, JSON.stringify({ ...JSON.parse(template), bootstrap_root: false }));
  await assert.rejects(initializeWorkspace(options), /declared bootstrap-root/);
  await fs.writeFile(templatePath, JSON.stringify({ ...JSON.parse(template), allowed_child_ee_templates: [] }));
  await assert.rejects(initializeWorkspace(options), /does not allow template 'project'/);
  await fs.writeFile(templatePath, template);
  let drifted = false;
  await assert.rejects(initializeWorkspace({ ...options, planOperatorSurface: async (input) => {
    if (!drifted) { drifted = true; await fs.writeFile(rootPath, `${before}\n`); }
    return fixturePlan(home, registry, input);
  } }), /expected .*found/);
  assert.equal(await fs.readFile(rootPath, "utf8"), `${before}\n`);
  await fs.writeFile(rootPath, before);
  const open = fs.open;
  let failed = false;
  fs.open = async (file, flags, mode) => {
    if (!failed && flags === "wx" && file.endsWith(`${path.sep}interface.json`)) { failed = true; throw new Error("fixture publication write failed"); }
    return open(file, flags, mode);
  };
  try { await assert.rejects(initializeWorkspace(options), /fixture publication write failed/); }
  finally { fs.open = open; }
  assert.equal(failed, true);
  assert.equal(await fs.readFile(rootPath, "utf8"), before);
  assert.deepEqual(await fs.readdir(path.join(home, "state", "runs")), []);
  assert.deepEqual(await fs.readdir(path.dirname(rootPath)), ["ee_existence.json"]);
  const outcome = await initializeWorkspace(options), state = await readWorkspaceSetupState(options);
  assert.equal(outcome.rootEeId, root.ee_id);
  assert.deepEqual(JSON.parse(await fs.readFile(rootPath)), { ...root, goal: draft.spaceGoal });
  assert.equal(state.receipt.request_digest, await workspaceInitializeRequestDigest(request));
  assert.equal(state.status, "configured");
  assert.equal((await fs.readdir(path.dirname(rootPath))).length, request.projects.length + 1);
  const projectIds = new Map(outcome.projects.map((project) => [project.key, project.eeId]));
  for (const project of request.projects) {
    const record = JSON.parse(await fs.readFile(path.join(path.dirname(rootPath), `${projectIds.get(project.key)}.json`)));
    assert.equal(record.parent_ee_id, project.parentKey === "root" ? root.ee_id : projectIds.get(project.parentKey));
    assert.equal(record.goal, project.goal);
  }
  const sidecar = JSON.parse(await fs.readFile(path.join(registry, "interface", outcome.homeInterfaceId, "octopus_os.json")));
  assert.equal(sidecar.presentation.name, draft.spaceName);
  assert.deepEqual(sidecar.presentation.projects, request.projects.map((project) => ({ ee_id: projectIds.get(project.key), name: project.name, ...project.presentation })));
  assert.equal((await initializeWorkspace(options)).changed, false);
  await fs.writeFile(rootPath, JSON.stringify({ ...root, parent_ee_id: "foreign_root" }));
  assert.equal((await readWorkspaceSetupState(options)).status, "degraded");
}

async function fixturePlan(octopusHomeRoot, registryRoot, { eeInstancesRoot, request }) {
  const rootRecord = JSON.parse(await fs.readFile(path.join(eeInstancesRoot, `${request.eeId}.json`), "utf8"));
  const clientSurfaceRole = request.clientSurfaceRole ?? "home";
  if (clientSurfaceRole === "home") {
    assert.equal(rootRecord.parent_ee_id, null, "Home must live directly in the root Existence EE; select its actual root EE");
    const template = JSON.parse(await fs.readFile(path.join(registryRoot, "ee_template", rootRecord.template_name, "ee_template.json")));
    assert.equal(template.bootstrap_root, true, "Home requires the declared bootstrap-root EE");
  } else assert.equal(rootRecord.template_name, "project");
  const runId = `run_operator_chat_${request.chatId}`;
  const chatInterfaceId = `world_operator_chat_${request.chatId}`;
  const modelInterfaceId = `${request.providerInterfaceId}_${request.chatId}`;
  const workerChoiceInterfaceId = request.workerInterfaceId;
  const workerInterfaceId = `${workerChoiceInterfaceId}_${request.chatId}`;
  const toolId = `file.write__authoring_${request.chatId}`;
  const intakeId = "system_service_input_5c4a3a35e6f2f0f23f7bb5ae";
  const writerId = "system_input_writer_5c4a3a35e6f2f0f23f7bb5ae";
  const inputToolId = "file.write.input_5c4a3a35e6f2f0f23f7bb5ae";
  const requiredInterfaceIds = [chatInterfaceId, modelInterfaceId, workerInterfaceId, intakeId, writerId].sort();
  const requiredToolIds = [toolId, inputToolId].sort();
  const authoringProfile = {
    ee_id: request.eeId,
    worker_choice_interface: workerChoiceInterfaceId,
    door_host_slots: request.doorHostSlots ?? {},
    ...(request.contextArtifactBindings?.length ? { context_artifact_bindings: request.contextArtifactBindings } : {}),
    ...(request.payloadArtifactBindings?.length ? { payload_artifact_bindings: request.payloadArtifactBindings } : {}),
    ...(request.selectedWorkerEnvironmentKinds?.length
      ? { selected_worker_environment_kinds: request.selectedWorkerEnvironmentKinds }
      : {}),
  };
  const interfaceRoot = path.join(registryRoot, "interface", chatInterfaceId);
  const sidecar = {
    schema_version: 1,
    workspace: "octopus_os",
    interface: chatInterfaceId,
    owner_interface: chatInterfaceId,
    component: "client",
    surface_role: clientSurfaceRole,
    provider_choice_interface: request.providerInterfaceId,
    worker_choice_interface: workerChoiceInterfaceId,
    authoring_profile: authoringProfile,
  };
  const interfaceBinding = (interfaceId) => ({
    interface: interfaceId,
    config: { artifact_policy: { mode: "all" } },
  });
  const checkpoints = [
    checkpoint("cp_home_input", chatInterfaceId, [modelInterfaceId]),
    checkpoint("cp_worker_input", workerInterfaceId, [modelInterfaceId]),
    checkpoint("cp_home_output", modelInterfaceId, [chatInterfaceId]),
    checkpoint("cp_intake_input", intakeId, [modelInterfaceId]),
    checkpoint("cp_intake_call", intakeId, [writerId]),
    checkpoint("cp_intake_result", writerId, [intakeId]),
  ];
  const artifactId = `artifact.fixture.home.${request.chatId}`;
  const artifactWrites = clientSurfaceRole === "home" ? [write(path.join(registryRoot, "artifact", artifactId, "artifact.json"),
    `${JSON.stringify({ id: artifactId, label: "context.reference", text: "Exact fixture Home context" })}\n`, `artifact:${artifactId}`)] : [];
  if (artifactWrites.length) checkpoints[0].artifact_bindings = [{ id: artifactId }];
  checkpoints[4].required_verified_artifacts = [{ artifact_label: "tool.call", mark: "tool.call.allowed", verifier: "tool_call_allowed", params: { tools: [inputToolId], metadata_matches: [{ payload: "/arguments/expected_before_hash", metadata: "/runtime/run_record_hash" }] } }];
  const runRecord = {
    id: runId,
    ee: request.eeId,
    worker: workerInterfaceId,
    trace: `trace:${runId}`,
    status: "running",
    interface_state: {
      [chatInterfaceId]: {
        client_surface_role: clientSurfaceRole,
        provider_choice_interface: request.providerInterfaceId,
        provider_runtime_interface: modelInterfaceId,
        worker_choice_interface: workerChoiceInterfaceId,
      },
    },
    checkpoints,
    active_checkpoints: checkpoints.map((entry) => entry.id),
  };
  function checkpoint(id, inbound, outbound) {
    return {
      id,
      run: runId,
      trace: `trace:${runId}:${id}`,
      epoch: 0,
      interface_binding: interfaceBinding(inbound),
      outbound_interface_bindings: outbound.map(interfaceBinding),
      artifact_bindings: [],
      required_verified_artifacts: [],
    };
  }
  return {
    runId,
    chatInterfaceId,
    modelInterfaceId,
    providerModel: request.providerModel,
    workerInterfaceId,
    clientSurfaceRole,
    reasoningEffort: request.reasoningEffort ?? "high",
    authoringProfile,
    requiredInterfaceIds,
    requiredToolIds,
    writes: [
      ...artifactWrites,
      write(path.join(interfaceRoot, "interface.json"), `${JSON.stringify({ id: chatInterfaceId })}\n`, `interface:${chatInterfaceId}`),
      write(path.join(interfaceRoot, "octopus_os.json"), `${JSON.stringify(sidecar, null, 2)}\n`, `octopus-os:${chatInterfaceId}`),
      write(path.join(registryRoot, "interface", modelInterfaceId, "interface.json"), `${JSON.stringify({ id: modelInterfaceId, interface_driver_config: { private_config: { model: request.providerModel } } })}\n`, `interface:${modelInterfaceId}`),
      write(path.join(registryRoot, "interface", workerInterfaceId, "interface.json"), `${JSON.stringify({ id: workerInterfaceId })}\n`, `interface:${workerInterfaceId}`),
      write(path.join(registryRoot, "interface", workerInterfaceId, "policy.json"), `${JSON.stringify({ id: workerInterfaceId, ceiling: { interfaces: requiredInterfaceIds, tools: requiredToolIds } })}\n`, `policy:${workerInterfaceId}`),
      write(path.join(registryRoot, "interface", intakeId, "interface.json"), JSON.stringify({ id: intakeId, role: "service" }), `interface:${intakeId}`),
      write(path.join(registryRoot, "interface", intakeId, "life.json"), JSON.stringify({}), `life:${intakeId}`),
      write(path.join(registryRoot, "interface", writerId, "interface.json"), JSON.stringify({ id: writerId, role: "system" }), `interface:${writerId}`),
      write(path.join(registryRoot, "tool", toolId, "tool.json"), `${JSON.stringify({ id: toolId, interface: writerId })}\n`, `tool:${toolId}`),
      write(path.join(registryRoot, "tool", inputToolId, "tool.json"), JSON.stringify({ id: inputToolId, interface: writerId }), `tool:${inputToolId}`),
      write(path.join(octopusHomeRoot, "state", "runs", `${runId}.json`), `${JSON.stringify(runRecord, null, 2)}\n`, `run:${runId}`),
    ],
    ledgerOffset: 0,
  };
}

async function fixtureProjectScope(octopusHomeRoot, registryRoot, workspace) {
  const environments = [];
  const eeRoot = path.join(octopusHomeRoot, "state", "ee_instances");
  for (const entry of await fs.readdir(eeRoot, { withFileTypes: true })) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const bytes = await fs.readFile(path.join(eeRoot, entry.name));
    const record = JSON.parse(bytes.toString("utf8"));
    const template = JSON.parse(await fs.readFile(path.join(registryRoot, "ee_template", record.template_name, "ee_template.json"), "utf8"));
    environments.push({
      eeId: record.ee_id,
      parentEeId: record.parent_ee_id,
      templateName: record.template_name,
      recordDigest: crypto.createHash("sha256").update(bytes).digest("hex"),
      allowedChildTemplateIds: [...template.allowed_child_ee_templates].sort(),
    });
  }
  const sidecar = await fs.readFile(path.join(registryRoot, "interface", workspace.homeInterfaceId, "octopus_os.json"));
  return {
    status: "ready",
    rootEeId: workspace.rootEeId,
    homeRunId: workspace.homeRunId,
    homeInterfaceId: workspace.homeInterfaceId,
    homeSidecarDigest: crypto.createHash("sha256").update(sidecar).digest("hex"),
    environments,
    clientRuns: [{ runId: workspace.homeRunId, eeId: workspace.rootEeId }],
    readOnlyRuns: [],
  };
}

function fixtureWorkspaceIdentity(scope) {
  return {
    rootEeId: scope.rootEeId,
    homeRunId: scope.homeRunId,
    homeInterfaceId: scope.homeInterfaceId,
    expectedHomeSidecarDigest: scope.homeSidecarDigest,
  };
}

async function createFixture(octopusHomeRoot, registryRoot) {
  for (const directory of [
    path.join(octopusHomeRoot, "state", "ee_instances"),
    path.join(registryRoot, "interface"),
    path.join(registryRoot, "tool"),
    path.join(registryRoot, "artifact"),
  ]) await fs.mkdir(directory, { recursive: true });
  await fs.writeFile(path.join(octopusHomeRoot, "state", "ee_instances", "ee_existence.json"), `${JSON.stringify({
    ee_id: "ee_existence",
    template_name: "existence",
    parent_ee_id: null,
    goal: "existence",
    config: {},
  }, null, 2)}\n`);
  await writeTemplate(registryRoot, "existence", ["project"]);
  await writeTemplate(registryRoot, "operator_space", ["project"]);
  await writeTemplate(registryRoot, "project", ["project"]);
}

async function writeTemplate(registryRoot, id, children) {
  const directory = path.join(registryRoot, "ee_template", id);
  await fs.mkdir(directory, { recursive: true });
  await fs.writeFile(path.join(directory, "ee_template.json"), `${JSON.stringify({
    id,
    goal_class: id,
    allowed_interfaces: null,
    allowed_worker_environment_kinds: null,
    allowed_child_ee_templates: children,
    bootstrap_root: id === "existence",
  }, null, 2)}\n`);
}

function setupRequest(operationId) {
  return {
    action: "initialize_workspace",
    operationId,
    root: { key: "root", templateName: "existence", name: "Founder", goal: "Operate life and work" },
    home: {
      name: "Home",
      providerInterfaceId: "world_provider_codex",
      providerModel: "fixture-model",
      workerInterfaceId: "system_worker_coder_max",
      reasoningEffort: "high",
      doorHostSlots: {},
      contextArtifactBindings: [{ id: "artifact-grounding-system-architecture" }],
      selectedWorkerEnvironmentKinds: ["git_worktree"],
    },
    projects: [
      project("life", "root", "Life", "globe", "teal"),
      project("work", "root", "Work", "briefcase", "indigo"),
      project("product", "work", "Product", "rocket", "violet"),
    ],
  };
}

function project(key, parentKey, name, icon, color) {
  return { key, parentKey, templateName: "project", name, goal: name, presentation: { icon, color } };
}

function write(filePath, content, tag) {
  return { filePath, content, tag, expectedBeforeHash: null };
}

function deterministicId(prefix, operationId, key) {
  return `${prefix}_${crypto.createHash("sha256").update(`${operationId}\u0000${key}`).digest("hex").slice(0, 24)}`;
}

const execution = process.argv[2] === "task-archive"
  ? fs.mkdtemp(path.join(os.tmpdir(), "octopus-task-archive-test-")).then(async (temporary) => {
      try { await verifyTaskArchive(temporary); } finally { await fs.rm(temporary, { recursive: true, force: true }); }
    })
  : process.argv[2] === "capability-preferences"
  ? verifyCapabilityPreferenceSetup()
  : process.argv[2] === "consent-setup"
  ? verifyRetiredConsentSetup()
  : process.argv[2] === "crash-child"
  ? crashChild(process.argv[3], process.argv[4])
  : process.argv[2] === "project-crash-child"
    ? projectCrashChild(...process.argv.slice(3))
    : main();

execution.catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
