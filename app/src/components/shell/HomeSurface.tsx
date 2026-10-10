import { lazy, Suspense, useEffect, type ComponentProps } from "react";
import { AnimatePresence, motion, useIsPresent, useReducedMotion } from "motion/react";
import { useHomeRunActivity } from "../voice/useHomeActivityWorkspace";
import type { SurfaceHostProps } from "./SurfaceHost";
import { SurfaceLoadingState } from "./SurfaceBoundary";

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
  | "homeWidgetsOpen"
  | "onHomeWidgetsOpenChange"
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
  homeWidgetsOpen = false,
  onHomeWidgetsOpenChange,
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
  const reducedMotion = useReducedMotion();
  const transition = { duration: reducedMotion ? 0 : 0.22, ease: [0.23, 1, 0.32, 1] as const };
  useEffect(() => { void loadHomeAgentActivityView().catch(() => undefined); }, []);
  return (
    <div className="home-view-transition">
      <motion.div
        animate={{ opacity: activityOpen ? 0 : 1 }}
        aria-hidden={activityOpen}
        className="home-view-transition__surface"
        inert={activityOpen}
        initial={false}
        transition={transition}
      >
        <VoiceView accessProfile={accessProfile} activity={homeActivity} builtinWidgetSources={builtinVoiceWidgetSources} commandsActive={commandsActive && homeViewMode === "voice"} homeRunId={homeRunId} installedWidgetIds={installedVoiceWidgetIds} installedWidgetsReady={installedVoiceWidgetsReady} onViewModeChange={onHomeViewModeChange} onWidgetsOpenChange={onHomeWidgetsOpenChange} session={voiceSession} surfaces={voiceSurfaces} topology={topology} workerEnvironmentSurfaces={workerEnvironmentSurfaces} widgetsOpen={homeWidgetsOpen} workspace={chatWorkspace} viewMode="voice" />
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
