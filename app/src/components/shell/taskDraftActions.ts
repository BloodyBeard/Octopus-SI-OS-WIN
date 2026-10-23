import type { ChatWorkspace } from "../chat";

export function taskDraftActions(
  source: Pick<ChatWorkspace, "renderedMessages">,
  target: Pick<ChatWorkspace, "continueInNewTask" | "editUserMessageInNewTask">,
  openDraft: () => void,
): Pick<ChatWorkspace, "continueInNewTask" | "editUserMessageInNewTask"> {
  return {
    continueInNewTask(message) {
      const created = target.continueInNewTask(message);
      if (created) openDraft();
      return created;
    },
    editUserMessageInNewTask(message, text) {
      const created = target.editUserMessageInNewTask(message, text, source.renderedMessages);
      if (created) openDraft();
      return created;
    },
  };
}
