import { OcDialog, OcIconButton } from "../../design-system/components";
import { appToast } from "../../toasts";
import { ArtifactPreview } from "../artifacts/ArtifactPreview";
import { openArtifactFile, type ArtifactFile } from "../artifacts/artifactFileOperations";
import type { ArtifactPreviewModel } from "../artifacts/types";
import "../artifacts/artifact-workbench.css";

export function ChatAttachmentFilePreview({ file, model, error, onClose }: {
  file: ArtifactFile; model?: ArtifactPreviewModel; error?: string; onClose: () => void;
}) {
  return <OcDialog ariaLabel={`Preview ${file.filename}`} open onClose={onClose} panelClassName="chat-attachment-file-preview" size="editor">
    <section aria-label={`Preview ${file.filename}`} className="artifact-workbench is-embedded">
      <header className="artifact-workbench-header">
        <div className="artifact-workbench-identity"><h2 title={file.filename}>{file.filename}</h2></div>
        <div className="artifact-workbench-actions">
          <OcIconButton glyph="openExternal" label="Open file" onClick={() => {
            void openArtifactFile(file, "open").catch((error: unknown) => appToast.danger(error instanceof Error ? error.message : "The file could not be opened."));
          }} size="xs" variant="ghost" />
          <OcIconButton glyph="close" label="Close preview" onClick={onClose} size="xs" variant="ghost" />
        </div>
      </header>
      <div className="artifact-workbench-content">{model ? <ArtifactPreview model={model} zoomPercent={100} />
        : <div className="artifact-preview-unavailable" role="alert"><strong>Preview unavailable</strong><span>{error}</span></div>}</div>
    </section>
  </OcDialog>;
}
