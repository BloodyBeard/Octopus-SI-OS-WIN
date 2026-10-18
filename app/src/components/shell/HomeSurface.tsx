import { lazy, Suspense, useEffect, useState, type ComponentProps } from "react";
import { AnimatePresence, motion, useIsPresent, useReducedMotion } from "motion/react";
import { useHomeRunActivity } from "../voice/useHomeActivityWorkspace";
import type { SurfaceHostProps } from "./SurfaceHost";
import { SurfaceLoadingState } from "./SurfaceBoundary";
import { WorkspacePaneHeaderActions } from "./WorkspacePaneHeaderActions";
import { ShellTooltip } from "./ShellTooltip";
import { Glyph } from "../IconGlyphs";

const VoiceView = lazy(() => import("../voice/VoiceView").then((module) => ({ default: module.VoiceView })));
const loadHomeAgentActivityView = () => import("../voice/HomeAgentActivityView").then((module) => ({ default: module.HomeAgentActivityView }));
const HomeAgentActivityView = lazy(loadHomeAgentActivityView);

type Props = Pick<SurfaceHostProps,
  "accessProfile"
  | "commandsActive"
  | "homeRunId"
  | "voiceSession"
  | "homeViewMode"
  | "onHomeViewModeChange"
  | "searchFocusTick"
  | "chatWorkspace"
  | "onOpenFiles"
  | "voiceSurfaces"
  | "installedVoiceWidgetIds"
  | "installedVoiceWidgetsReady"
  | "builtinVoiceWidgetSources"
  | "workerEnvironmentSurfaces"
  | "topology"
>;

export function HomeSurface({
  accessProfile,
  commandsActive = true,
  homeRunId,
  voiceSession,
  homeViewMode,
  onHomeViewModeChange,
  searchFocusTick,
  chatWorkspace,
  onOpenFiles,
  voiceSurfaces,
  installedVoiceWidgetIds,
  installedVoiceWidgetsReady,
  builtinVoiceWidgetSources,
  workerEnvironmentSurfaces,
  topology,
}: Props) {
  const homeActivity = useHomeRunActivity(homeRunId, true);
  const activityOpen = homeViewMode === "activity";
  const [widgetsOpen, setWidgetsOpen] = useState(false);
  useEffect(() => { if (activityOpen) setWidgetsOpen(false); }, [activityOpen]);
  const reducedMotion = useReducedMotion();
  const transition = { duration: reducedMotion ? 0 : 0.22, ease: [0.23, 1, 0.32, 1] as const };
  useEffect(() => { void loadHomeAgentActivityView().catch(() => undefined); }, []);
  return (
    <div className="home-view-transition">
      <WorkspacePaneHeaderActions>
        <ShellTooltip label="Activity" side="right">
          <button aria-label="Activity" aria-expanded={activityOpen} className="app-titlebar-action no-drag"
            data-state={activityOpen ? "open" : "closed"} type="button" onClick={() => {
              setWidgetsOpen(false);
              onHomeViewModeChange?.(activityOpen ? "voice" : "activity");
            }}><Glyph name="transcript" size={16} /></button>
        </ShellTooltip>
        {!activityOpen ? (
          <ShellTooltip label="Home widgets">
            <button aria-label="Home widgets" aria-expanded={widgetsOpen} className="app-titlebar-action no-drag"
              data-state={widgetsOpen ? "open" : "closed"} type="button" onClick={() => {
                onHomeViewModeChange?.("voice");
                setWidgetsOpen((open) => !open);
              }}><Glyph name="widgets" size={16} /></button>
          </ShellTooltip>
        ) : null}
      </WorkspacePaneHeaderActions>
      <motion.div
        animate={{ opacity: activityOpen ? 0 : 1 }}
        aria-hidden={activityOpen}
        className="home-view-transition__surface"
        inert={activityOpen}
        initial={false}
        transition={transition}
      >
        <VoiceView accessProfile={accessProfile} activity={homeActivity} builtinWidgetSources={builtinVoiceWidgetSources} commandsActive={commandsActive && homeViewMode === "voice"} homeRunId={homeRunId} installedWidgetIds={installedVoiceWidgetIds} installedWidgetsReady={installedVoiceWidgetsReady} onViewModeChange={onHomeViewModeChange} onWidgetsOpenChange={setWidgetsOpen} session={voiceSession} surfaces={voiceSurfaces} topology={topology} workerEnvironmentSurfaces={workerEnvironmentSurfaces} widgetsOpen={widgetsOpen} workspace={chatWorkspace} viewMode="voice" />
      </motion.div>
      <div aria-hidden={!activityOpen} className="home-view-transition__surface" inert={!activityOpen}>
        <AnimatePresence initial={false}>
          {activityOpen ? (
            <motion.div
              animate={{ opacity: 1 }}
              className="home-view-transition__surface"
              exit={{ opacity: 0 }}
              initial={{ opacity: 0 }}
              key="agent-activity"
              transition={transition}
            >
              <Suspense fallback={<SurfaceLoadingState label="Loading Home activity" />}>
                <HomeActivityContent
                  commandsActive={commandsActive && homeViewMode === "activity"}
                  onOpenFiles={onOpenFiles}
                  searchFocusTick={searchFocusTick}
                  surfaces={workerEnvironmentSurfaces}
                  topology={topology}
                  workspace={chatWorkspace}
                />
              </Suspense>
            </motion.div>
          ) : null}
        </AnimatePresence>
      </div>
    </div>
  );
}

function HomeActivityContent(props: ComponentProps<typeof HomeAgentActivityView>) {
  const present = useIsPresent();
  return <HomeAgentActivityView {...props} commandsActive={props.commandsActive && present} />;
}
