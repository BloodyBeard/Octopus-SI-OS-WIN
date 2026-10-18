import { createContext, useCallback, useContext, useState, type ReactNode } from "react";
import type { OctopusScheduledMutationRequest } from "../../electron";
import { canMutateScheduledInterfaces, mutateScheduledInterface } from "../../projectionClient";
import { draftFromScheduledInterface, draftFromTemplate, mutationDraft } from "./scheduleModel";
import type { ScheduledSuggestion } from "./scheduledSuggestions";
import type { ScheduledEditorDraft, ScheduledEditorMode, ScheduledInterfaceProjection } from "./types";
import { useScheduledInterfaces } from "./useScheduledInterfaces";

function useScheduledWorkspaceState(active: boolean) {
  const projection = useScheduledInterfaces(active);
  const [selectedId, setSelectedId] = useState("");
  const [editor, setEditor] = useState<{ mode: ScheduledEditorMode; draft: ScheduledEditorDraft; revision: number } | null>(null);
  const [pending, setPending] = useState(false);
  const [mutationError, setMutationError] = useState("");
  const [searchRequest, setSearchRequest] = useState(0);
  const requestSearch = useCallback(() => setSearchRequest((current) => current + 1), []);
  const canEdit = canMutateScheduledInterfaces() && !projection.error;
  const templates = projection.tasks.filter((task) => task.valid && task.cloneEligible);
  const selected = projection.tasks.find((task) => task.interfaceId === selectedId) ?? null;
  const createUnavailableReason = !canEdit ? "Open a connected desktop client to create a trigger."
    : projection.loading ? "Loading triggers…"
      : templates.length === 0 ? "A complete trigger template is needed before creating a task." : "";

  async function mutate(request: OctopusScheduledMutationRequest, after: () => void) {
    setPending(true);
    setMutationError("");
    try {
      await mutateScheduledInterface(request);
      after();
      await projection.reload();
    } catch (failure) {
      setMutationError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setPending(false);
    }
  }

  function selectTask(interfaceId: string) {
    if (pending) return;
    setSelectedId(interfaceId);
    setEditor(null);
    setMutationError("");
  }

  function startCreate(suggestion?: ScheduledSuggestion) {
    const source = templates[0];
    if (pending || createUnavailableReason || !source) return;
    const draft = draftFromTemplate(source);
    setEditor((previous) => ({
      mode: { kind: "create", source },
      draft: suggestion ? { ...draft, instructions: suggestion.instructions, type: "cron", cron: suggestion.cron } : draft,
      revision: (previous?.revision ?? 0) + 1,
    }));
    setMutationError("");
  }

  function startEdit(task: ScheduledInterfaceProjection) {
    if (pending || !canEdit || !task.valid) return;
    setEditor((previous) => ({ mode: { kind: "edit", task }, draft: draftFromScheduledInterface(task), revision: (previous?.revision ?? 0) + 1 }));
    setMutationError("");
  }

  function save(draft: ScheduledEditorDraft, source: ScheduledInterfaceProjection) {
    if (!editor || pending || !canEdit) return;
    const request: OctopusScheduledMutationRequest = editor.mode.kind === "create"
      ? { action: "create", sourceInterfaceId: source.interfaceId, sourceActivationDigest: source.activationDigest, interfaceId: draft.interfaceId, draft: mutationDraft(draft) }
      : { action: "update", interfaceId: editor.mode.task.interfaceId, expectedDigest: editor.mode.task.activationDigest, draft: mutationDraft(draft) };
    void mutate(request, () => { setSelectedId(draft.interfaceId); setEditor(null); });
  }

  function toggleTask(task: ScheduledInterfaceProjection) {
    if (pending || !canEdit || !task.valid) return;
    void mutate({ action: "set_enabled", interfaceId: task.interfaceId, expectedDigest: task.activationDigest, enabled: task.status === "disabled" }, () => {});
  }

  function deleteTask(task: ScheduledInterfaceProjection) {
    if (pending || !canEdit) return;
    void mutate({ action: "delete", interfaceId: task.interfaceId, expectedDigest: task.activationDigest }, () => {
      if (selectedId === task.interfaceId) setSelectedId("");
    });
  }

  return {
    ...projection, selected, selectedId, selectTask, editor, templates, pending, canEdit, createUnavailableReason,
    mutationError, dismissError: () => setMutationError(""), startCreate, startEdit, save, toggleTask, deleteTask,
    cancelEdit: () => { if (!pending) { setEditor(null); setMutationError(""); } },
    searchRequest, requestSearch,
  };
}

const ScheduledWorkspaceContext = createContext<ReturnType<typeof useScheduledWorkspaceState> | null>(null);

export function ScheduledWorkspaceProvider({ active, children }: { active: boolean; children: ReactNode }) {
  const state = useScheduledWorkspaceState(active);
  return <ScheduledWorkspaceContext.Provider value={state}>{children}</ScheduledWorkspaceContext.Provider>;
}

export function useScheduledWorkspace() {
  const state = useContext(ScheduledWorkspaceContext);
  if (!state) throw new Error("Scheduled must be rendered inside its workspace provider.");
  return state;
}
