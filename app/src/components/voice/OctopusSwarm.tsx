import { useEffect, useRef } from "react";
import { shouldReduceMotion } from "../../appearance";
import { projectColor } from "../../design-system/projectColors";
import type { ChatRun } from "../../chatRuns";
import { createOctopusOcean, type OceanCreature, type OctopusOcean } from "./octopusOcean";

const SWARM_CANVAS_STYLE = { position: "absolute", inset: 0, width: "100%", height: "100%", display: "block", pointerEvents: "none" } as const;
const SWARM_LIMIT = 7;

export function OctopusSwarm({ runs }: { runs: readonly ChatRun[] }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const oceanRef = useRef<OctopusOcean | null>(null);
  const creatures = swarmCreatures(runs);
  const signature = JSON.stringify(creatures);
  const creaturesRef = useRef(creatures);
  creaturesRef.current = creatures;

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ocean = createOctopusOcean(canvas, "swarm", shouldReduceMotion());
    oceanRef.current = ocean;
    ocean.set({ creatures: creaturesRef.current });
    return () => {
      ocean.destroy();
      oceanRef.current = null;
    };
  }, []);

  useEffect(() => {
    oceanRef.current?.set({ creatures: creaturesRef.current });
  }, [signature]);

  return <canvas aria-hidden="true" className="octopus-swarm" ref={canvasRef} style={SWARM_CANVAS_STYLE} />;
}

function swarmCreatures(runs: readonly ChatRun[]): OceanCreature[] {
  return runs
    .filter((run) => run.clientSurfaceRole === "task" && !run.archived)
    .sort((left, right) => Number(right.turnState === "working") - Number(left.turnState === "working") || right.updatedAtMs - left.updatedAtMs)
    .slice(0, SWARM_LIMIT)
    .map((run) => ({
      id: run.runId,
      color: run.agentColor ? projectColor(run.agentColor) : null,
      activity: run.turnState === "working" ? 1 : run.turnState === "waiting" ? 0.35 : 0,
    }));
}
