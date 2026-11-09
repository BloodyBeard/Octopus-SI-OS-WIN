import { openArtifactInFiles } from "../files/FilesPaneNavigation";
import type { FilesOpenTarget } from "../files/types";
import type { ArtifactReference } from "../artifacts/types";
import { Glyph } from "../IconGlyphs";
import type { TaskSourceRow } from "./taskSourcesPane";

export function TaskSourcesPaneTab({ sources, onOpenFiles }: { sources: TaskSourceRow[]; onOpenFiles: (target: FilesOpenTarget) => void }) {
  return (
    <div className="task-sources-panel">
      <ul aria-label="Sources">
        {sources.map((source) => (
          <li key={source.id}>
            {source.reference ? (
              <button aria-label={source.label} onClick={() => openArtifactInFiles(source.reference as ArtifactReference, onOpenFiles)} type="button">
                <SourceIdentity source={source} />
              </button>
            ) : <div><SourceIdentity source={source} /></div>}
            <div className="task-source-activity">
              <span>{source.activity}</span>
              {source.details?.length ? (
                <ul>
                  {source.details.map((detail) => <li key={detail}>{detail}</li>)}
                </ul>
              ) : null}
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

function SourceIdentity({ source }: { source: TaskSourceRow }) {
  return (
    <>
      <span className="task-source-icon"><Glyph name={source.glyph} size={16} /></span>
      <span className="task-source-copy"><strong>{source.label}</strong>{source.meta ? <small>{source.meta}</small> : null}</span>
    </>
  );
}
