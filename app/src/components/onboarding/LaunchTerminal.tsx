import { useEffect, useMemo, useState } from "react";
import { useShouldReduceMotion } from "../../appearance";
import { OctopusTerminal } from "../terminal/OctopusTerminal";
import { createLaunchShellSession } from "../terminal/shellSession";
import { OctopusLoadingLogo } from "../OctopusLoadingLogo";

export function LaunchTerminal({
  greeting,
  leaving,
  onExited,
}: {
  greeting: boolean;
  leaving: boolean;
  onExited: () => void;
}) {
  const reduceMotion = useShouldReduceMotion();
  const [phase, setPhase] = useState<"greeting" | "terminal">(greeting ? "greeting" : "terminal");
  const session = useMemo(() => createLaunchShellSession(), []);

  useEffect(() => {
    if (phase !== "greeting") return;
    const timer = setTimeout(() => setPhase("terminal"), reduceMotion ? 700 : 1700);
    return () => clearTimeout(timer);
  }, [phase, reduceMotion]);

  useEffect(() => {
    if (!leaving || phase === "greeting") return;
    const timer = setTimeout(onExited, reduceMotion ? 0 : 480);
    return () => clearTimeout(timer);
  }, [leaving, onExited, phase, reduceMotion]);

  const exitVisible = leaving && phase !== "greeting";

  return (
    <main aria-label="Octopus launch" className="launch-scene" data-leaving={exitVisible || undefined} data-phase={phase}>
      <div className="launch-scene-terminal">
        <OctopusTerminal id="terminal-panel-launch" session={session} />
      </div>
      {greeting ? (
        <div aria-hidden={phase !== "greeting"} className="launch-scene-greeting">
          <OctopusLoadingLogo />
        </div>
      ) : null}
      <div className="launch-scene-drag" />
    </main>
  );
}
