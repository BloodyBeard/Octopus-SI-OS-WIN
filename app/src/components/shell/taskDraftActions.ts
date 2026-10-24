import type { ChatWorkspace } from "../chat";

export function taskDraftActions(
  source: Pick<ChatWorkspace, "editUserMessage">,
  target: Pick<ChatWorkspace, "continueInNewTask" | "editUserMessage">,
  openDraft: () => void,
): Pick<ChatWorkspace, "continueInNewTask" | "editUserMessage"> {
  return {
    continueInNewTask(message) {
      const created = target.continueInNewTask(message);
      if (created) openDraft();
      return created;
    },
    editUserMessage: source.editUserMessage,
  };
}
