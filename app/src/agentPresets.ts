import type { ChatExecutionContext } from "./chatRuns";

export const AGENT_PRESET_ICONS = [
  "default",
  "strict",
  "calm",
  "curious",
  "glasses",
  "goggles",
  "visor",
  "eyepiece",
  "headphones",
  "headset",
  "headband",
  "hardhat",
] as const;

export const DEFAULT_AGENT_PRESET_ICON: ClientAgentPresetIcon = "default";

export type ClientAgentPresetIcon = typeof AGENT_PRESET_ICONS[number];

export type ClientAgentPreset = {
  id: string;
  name: string;
  icon: ClientAgentPresetIcon;
  projectEeId: string;
  workerInterfaceId: string;
  createdAtMs: number;
  updatedAtMs: number;
};

export type ClientAgentPresetInput = {
  name: string;
  icon: ClientAgentPresetIcon;
  projectEeId: string;
  workerInterfaceId: string;
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort: string;
  serviceTier?: string;
  applicationIds: string[];
  driverNames: string[];
};

export type ClientAgentDefinition = ClientAgentPreset & {
  deletedAtMs: number;
  agentCatalogDigest: string;
  sourceTemplate: boolean;
  clientTemplateDigest: string;
  providerInterfaceId: string;
  providerModel: string;
  reasoningEffort: string;
  serviceTier?: string;
  applicationIds: string[];
  driverNames: string[];
};

export function resolveClientAgents(contexts: readonly ChatExecutionContext[]): ClientAgentDefinition[] {
  const agents = new Map<string, ClientAgentDefinition>();
  for (const context of contexts) {
    for (const worker of context.workers) {
      if (!worker.clientAgentId) continue;
      const definition: ClientAgentDefinition = {
        id: worker.clientAgentId,
        name: worker.clientAgentName,
        icon: requiredAgentIcon(worker.clientAgentIcon),
        projectEeId: context.eeId,
        workerInterfaceId: worker.id,
        createdAtMs: worker.clientAgentCreatedAtMs,
        updatedAtMs: worker.clientAgentUpdatedAtMs,
        deletedAtMs: worker.clientAgentDeletedAtMs,
        agentCatalogDigest: context.agentCatalogDigest,
        sourceTemplate: worker.agentSourceTemplate,
        clientTemplateDigest: worker.clientTemplateDigest,
        providerInterfaceId: worker.clientTemplateProviderInterfaceId,
        providerModel: worker.clientTemplateProviderModel,
        reasoningEffort: worker.clientTemplateReasoningEffort,
        serviceTier: worker.clientTemplateServiceTier,
        applicationIds: [...worker.clientTemplateApplicationIds],
        driverNames: [...worker.clientTemplateWorkerEnvironmentKinds],
      };
      if (!definition.name || !definition.projectEeId || !definition.workerInterfaceId || !definition.agentCatalogDigest) {
        throw new Error(`Client Agent '${definition.id}' has an incomplete projected identity.`);
      }
      const existing = agents.get(definition.id);
      if (existing && JSON.stringify(existing) !== JSON.stringify(definition)) {
        throw new Error(`Client Agent '${definition.id}' is projected with conflicting identities.`);
      }
      agents.set(definition.id, definition);
    }
  }
  return [...agents.values()].sort((left, right) => (
    left.createdAtMs - right.createdAtMs
    || left.name.localeCompare(right.name)
    || left.id.localeCompare(right.id)
  ));
}

export function visibleClientAgents(agents: readonly ClientAgentDefinition[]): ClientAgentDefinition[] {
  return agents.filter((agent) => agent.deletedAtMs === 0 && Boolean(agent.clientTemplateDigest));
}

function requiredAgentIcon(value: string): ClientAgentPresetIcon {
  if (!(AGENT_PRESET_ICONS as readonly string[]).includes(value)) {
    throw new Error(`Client Agent icon '${value}' is not declared.`);
  }
  return value as ClientAgentPresetIcon;
}
