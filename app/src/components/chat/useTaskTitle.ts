import { useEffect, useRef } from "react";
import type { ChatRun } from "../../chatRuns";
import type { OctopusClientAccessProfile } from "../../electron";
import { useClientTaskOrganization } from "../../shell/clientWorkspaces";
import { appToast } from "../../toasts";

export function useTaskTitle(run: ChatRun | null | undefined, home: string, profile: OctopusClientAccessProfile, enabled: boolean) {
  const organization = useClientTaskOrganization(profile);
  const attempted = useRef(new Set<string>());
  const runId = run?.runId;
  const needed = run?.titleGenerationNeeded && !organization.aliases[runId ?? ""];
  useEffect(() => {
    if (!enabled || profile !== "live" || !runId || !needed || !window.octopusTransport?.requestTaskTitle) return;
    const key = JSON.stringify([home, runId]);
    if (attempted.current.has(key)) return;
    attempted.current.add(key);
    void window.octopusTransport.requestTaskTitle({ runId }).catch(() => {
      appToast.warning("Couldn't generate a task title. You can rename it in the sidebar.");
    });
  }, [enabled, home, needed, profile, runId]);
}
