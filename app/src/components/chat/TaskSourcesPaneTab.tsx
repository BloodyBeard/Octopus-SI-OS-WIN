import { useState } from "react";
import { ArtifactWorkbench } from "../artifacts/ArtifactWorkbench";
import type { ArtifactReference } from "../artifacts/types";
import { Glyph } from "../IconGlyphs";
import type { TaskSourceRow } from "./taskSourcesPane";

export function TaskSourcesPaneTab({ sources }: { sources: TaskSourceRow[] }) {
  const [artifactTarget, setArtifactTarget] = useState<ArtifactReference | null>(null);
  return (
    <div className="task-sources-panel">
      <ul aria-label="Sources">
        {sources.map((source) => (
          <li key={source.id}>
            {source.reference ? (
              <button aria-label={source.label} onClick={() => setArtifactTarget(source.reference as ArtifactReference)} type="button">
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
      {artifactTarget ? <ArtifactWorkbench onClose={() => setArtifactTarget(null)} presentation="overlay" reference={artifactTarget} /> : null}
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
