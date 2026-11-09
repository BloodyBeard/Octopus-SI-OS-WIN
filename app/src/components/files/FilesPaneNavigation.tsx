import { createContext, useCallback, useContext, useRef, type ReactNode } from "react";
import { appToast } from "../../toasts";
import type { ArtifactReference } from "../artifacts/types";
import { artifactFilesTarget } from "./artifactFilesTarget";
import type { FilesOpenTarget } from "./types";

const FilesPaneNavigation = createContext<((target: FilesOpenTarget) => void) | null>(null);

export function FilesPaneNavigationProvider({ children, onOpen }: { children: ReactNode; onOpen: (target: FilesOpenTarget) => void }) {
  const latest = useRef(onOpen);
  latest.current = onOpen;
  const open = useCallback((target: FilesOpenTarget) => latest.current(target), []);
  return <FilesPaneNavigation.Provider value={open}>{children}</FilesPaneNavigation.Provider>;
}

export function openArtifactInFiles(reference: ArtifactReference, onOpen: (target: FilesOpenTarget) => void): void {
  try { onOpen(artifactFilesTarget(reference)); }
  catch (error) { appToast.warning(error instanceof Error ? error.message : String(error)); }
}

export function useOpenArtifactFiles(): (reference: ArtifactReference) => void {
  const onOpen = useContext(FilesPaneNavigation);
  return useCallback((reference: ArtifactReference) => {
    if (!onOpen) { appToast.warning("The Files pane is unavailable in this view."); return; }
    openArtifactInFiles(reference, onOpen);
  }, [onOpen]);
}
