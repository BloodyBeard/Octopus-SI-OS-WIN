import type { ChatWorkspace } from "./useChatWorkspace";

export function chatProductError(chat: ChatWorkspace): string {
  return chat.error
    || chat.runsError
    || chat.executionContextError
    || chat.newTaskAuthoringError
    || chat.providerInterfaceError
    || ((chat.activeRunId || chat.agentPresetId)
      && chat.providerInterfacesLoaded
      && Boolean(chat.providerInterfaceId)
      && !chat.declaredProviderInterfaces.some((provider) => provider.id === chat.providerInterfaceId)
      ? `${chat.providerInterfaceId} is not declared in the provider registry projection`
      : "")
    || (!chat.artifactWriterAvailable ? "Artifact writer unavailable" : "");
}
