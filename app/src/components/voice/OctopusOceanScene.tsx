import { useEffect, useRef, type RefObject } from "react";
import { shouldReduceMotion } from "../../appearance";
import { createOctopusOcean, type OceanCreature, type OceanFocus } from "./octopusOcean";
import type { VoiceOrbActivity } from "./VoiceOrbScene";

const OCEAN_CANVAS_STYLE = { position: "absolute", inset: 0, width: "100%", height: "100%", display: "block" } as const;
const ORB_PROBE_SPHERE = 0.285;
const HOME_CREATURES = ["home-near", "home-middle", "home-far"];

type Props = {
  activity: VoiceOrbActivity;
  calling: boolean;
  orbProbe?: RefObject<HTMLElement | null>;
  rippleSurface?: RefObject<HTMLElement | null>;
};

export function OctopusOceanScene({ activity, calling, orbProbe, rippleSurface }: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const activityRef = useRef(activity);
  activityRef.current = activity;
  const callingRef = useRef(calling);
  callingRef.current = calling;
  const orbProbeRef = useRef(orbProbe);
  orbProbeRef.current = orbProbe;
  const rippleSurfaceRef = useRef(rippleSurface);
  const feedRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const reduceMotion = shouldReduceMotion();
    const ocean = createOctopusOcean(canvas, "home", reduceMotion, rippleSurfaceRef.current?.current ?? null);
    let raf = 0;
    const focus = (): OceanFocus | null => {
      const probe = orbProbeRef.current?.current;
      if (!probe || probe.clientWidth <= 0) return null;
      const probeRect = probe.getBoundingClientRect();
      const canvasRect = canvas.getBoundingClientRect();
      return {
        x: probeRect.left + probeRect.width / 2 - canvasRect.left,
        y: probeRect.top - canvasRect.top,
        radius: probeRect.width * ORB_PROBE_SPHERE,
      };
    };
    const feed = () => {
      ocean.set({
        creatures: homeCreatures(voiceLevels(activityRef.current, performance.now() / 1000)),
        focus: focus(),
        calling: callingRef.current,
      });
    };
    feedRef.current = feed;
    const loop = () => {
      feed();
      raf = requestAnimationFrame(loop);
    };
    if (reduceMotion) feed();
    else raf = requestAnimationFrame(loop);
    return () => {
      cancelAnimationFrame(raf);
      feedRef.current = null;
      ocean.destroy();
    };
  }, []);

  useEffect(() => {
    if (shouldReduceMotion()) feedRef.current?.();
  }, [calling]);

  return <canvas aria-hidden="true" className="octopus-ocean-scene" ref={canvasRef} style={OCEAN_CANVAS_STYLE} />;
}

type VoiceLevels = { listening: number; speaking: number; thinking: number };

function voiceLevels(activity: VoiceOrbActivity, seconds: number): VoiceLevels {
  if (activity.source === "live") {
    return { listening: activity.listeningLevel, speaking: activity.speakingLevel, thinking: activity.thinkingLevel };
  }
  return {
    listening: Math.max(0, Math.sin(seconds * 0.53)) * 0.5,
    speaking: Math.max(0, Math.sin(seconds * 0.37 + 1.3)) * 0.6,
    thinking: Math.sin(seconds * 0.21) > 0.55 ? 1 : 0,
  };
}

function homeCreatures(voice: VoiceLevels): OceanCreature[] {
  const activity = Math.max(voice.thinking, voice.speaking * 0.6);
  return HOME_CREATURES.map((id, depth) => ({
    id,
    color: null,
    activity: depth === 0 ? activity : activity * 0.5,
    speaking: depth === 0 ? voice.speaking : voice.speaking * 0.4,
  }));
}
