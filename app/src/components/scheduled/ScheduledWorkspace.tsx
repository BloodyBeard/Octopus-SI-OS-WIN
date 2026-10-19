import { createContext, useCallback, useContext, useState, type ReactNode } from "react";
import type { OctopusScheduledMutationRequest } from "../../electron";
import { canMutateScheduledInterfaces, mutateScheduledInterface } from "../../projectionClient";
import { appToast } from "../../toasts";
import { draftFromScheduledInterface, draftFromTemplate, mutationDraft } from "./scheduleModel";
import type { ScheduledSuggestion } from "./scheduledSuggestions";
import type { ScheduledDelivery, ScheduledEditorDraft, ScheduledEditorMode, ScheduledInterfaceProjection } from "./types";
import { useScheduledInterfaces } from "./useScheduledInterfaces";

function useScheduledWorkspaceState(active: boolean) {
  const projection = useScheduledInterfaces(active);
  const [selectedId, setSelectedId] = useState("");
  const [editor, setEditor] = useState<{ mode: ScheduledEditorMode; draft: ScheduledEditorDraft; revision: number } | null>(null);
  const [pending, setPending] = useState(false);
  const [editorError, setEditorError] = useState("");
  const [deleteCandidate, setDeleteCandidate] = useState<ScheduledInterfaceProjection | null>(null);
  const [undoCandidate, setUndoCandidate] = useState<ScheduledDelivery | null>(null);
  const [searchRequest, setSearchRequest] = useState(0);
  const requestSearch = useCallback(() => setSearchRequest((current) => current + 1), []);
  const canEdit = canMutateScheduledInterfaces() && !projection.error;
  const triggers = projection.tasks.filter((task) => !task.template);
  const templates = projection.tasks.filter((task) => task.valid && task.cloneEligible)
    .sort((left, right) => Number(right.template) - Number(left.template) || left.displayName.localeCompare(right.displayName));
  const timeTemplate = templates.find((task) => task.schedule?.type !== "inactivity") ?? null;
  const selected = triggers.find((task) => task.interfaceId === selectedId) ?? null;
  const createUnavailableReason = !canEdit ? "Open a connected desktop client to create a trigger."
    : projection.loading ? "Loading triggers…"
      : templates.length === 0 ? "A complete trigger template is needed before creating a trigger." : "";
  const timeSuggestionUnavailableReason = createUnavailableReason || (timeTemplate ? "" : "Install a time-based trigger template to use these suggestions.");

  async function mutate(request: OctopusScheduledMutationRequest, failureTitle: string, after: () => void) {
    setPending(true);
    setEditorError("");
    try {
      await mutateScheduledInterface(request);
      after();
      await projection.reload();
    } catch (failure) {
      const message = failure instanceof Error ? failure.message : String(failure);
      if (request.action === "create" || request.action === "update") setEditorError(message);
      else appToast.danger(`${failureTitle}: ${message}`);
    } finally {
      setPending(false);
    }
  }

  function selectTask(interfaceId: string) {
    if (pending) return;
    setSelectedId(interfaceId);
  }

  function startCreate(suggestion?: ScheduledSuggestion) {
    const source = suggestion?.templateInterfaceId
      ? templates.find((task) => task.interfaceId === suggestion.templateInterfaceId)
      : suggestion ? timeTemplate : timeTemplate ?? templates[0];
    if (pending || createUnavailableReason || !source) return;
    const draft = draftFromTemplate(source);
    const timeBased = source.schedule?.type !== "inactivity";
    setEditor((previous) => ({
      mode: { kind: "create", source },
      draft: suggestion?.templateInterfaceId ? draft
        : suggestion ? { ...draft, instructions: suggestion.instructions, type: "cron", cron: suggestion.cron }
          : { ...draft, instructions: "", ...(timeBased ? { type: "cron" as const, cron: "0 9 * * *" } : {}) },
      revision: (previous?.revision ?? 0) + 1,
    }));
    setEditorError("");
  }

  function startEdit(task: ScheduledInterfaceProjection) {
    if (pending || !canEdit || !task.valid) return;
    setEditor((previous) => ({ mode: { kind: "edit", task }, draft: draftFromScheduledInterface(task), revision: (previous?.revision ?? 0) + 1 }));
    setEditorError("");
  }

  function save(draft: ScheduledEditorDraft, source: ScheduledInterfaceProjection) {
    if (!editor || pending || !canEdit) return;
    const request: OctopusScheduledMutationRequest = editor.mode.kind === "create"
      ? { action: "create", sourceInterfaceId: source.interfaceId, sourceActivationDigest: source.activationDigest, interfaceId: draft.interfaceId, draft: mutationDraft(draft) }
      : { action: "update", interfaceId: editor.mode.task.interfaceId, expectedDigest: editor.mode.task.activationDigest, draft: mutationDraft(draft) };
    void mutate(request, editor.mode.kind === "create" ? "Could not create trigger" : "Could not update trigger", () => { setSelectedId(draft.interfaceId); setEditor(null); });
  }

  function toggleTask(task: ScheduledInterfaceProjection) {
    if (pending || !canEdit || !task.valid) return;
    void mutate({ action: "set_enabled", interfaceId: task.interfaceId, expectedDigest: task.activationDigest, enabled: task.status === "disabled" }, "Could not update trigger", () => {});
  }

  function confirmDelete() {
    const task = deleteCandidate;
    if (!task || pending || !canEdit) return;
    void mutate({ action: "delete", interfaceId: task.interfaceId, expectedDigest: task.activationDigest }, "Could not delete trigger", () => {
      setDeleteCandidate(null);
      if (selectedId === task.interfaceId) setSelectedId("");
    });
  }

  function confirmUndo() {
    const delivery = undoCandidate;
    if (!delivery?.journal?.undoable || pending || !canEdit) return;
    void mutate({ action: "undo_changes", runId: delivery.runId, expectedJournalDigest: delivery.journal.digest }, "Could not undo changes", () => setUndoCandidate(null));
  }

  return {
    ...projection, tasks: triggers, selected, selectedId, selectTask, editor, editorError, templates, pending, canEdit, createUnavailableReason,
    timeSuggestionUnavailableReason, startCreate, startEdit, save, toggleTask,
    deleteCandidate, requestDelete: (task: ScheduledInterfaceProjection) => { if (!pending && canEdit) setDeleteCandidate(task); },
    cancelDelete: () => { if (!pending) setDeleteCandidate(null); }, confirmDelete,
    undoCandidate, requestUndo: (delivery: ScheduledDelivery) => { if (!pending && delivery.journal?.undoable) setUndoCandidate(delivery); },
    cancelUndo: () => { if (!pending) setUndoCandidate(null); }, confirmUndo,
    cancelEdit: () => { if (!pending) { setEditor(null); setEditorError(""); } },
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
  if (!state) throw new Error("Triggers must be rendered inside their workspace provider.");
  return state;
}
