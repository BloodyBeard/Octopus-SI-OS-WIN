import { useState } from "react";
import { isSafeClientSurfaceVideoSource } from "../../electron/clientSurfaceDocumentSemantics.mjs";
import "./client-surface-video.css";

export function ClientSurfaceVideo({ source, title }: { source: string; title?: string }) {
  const [failed, setFailed] = useState(false);
  if (!isSafeClientSurfaceVideoSource(source)) {
    return <div className="client-surface-state" role="alert">Video unavailable: an HTTPS video link is required.</div>;
  }
  return (
    <figure className="client-surface-video">
      <video
        aria-label={title || "Video"}
        controls
        crossOrigin="anonymous"
        playsInline
        preload="metadata"
        src={source}
        onError={() => setFailed(true)}
        onLoadedMetadata={() => setFailed(false)}
      />
      <figcaption>
        {title ? <strong>{title}</strong> : null}
        <span>{new URL(source).hostname}</span>
      </figcaption>
      {failed ? <div role="alert">This video could not be loaded. Check the link and try another video.</div> : null}
    </figure>
  );
}
