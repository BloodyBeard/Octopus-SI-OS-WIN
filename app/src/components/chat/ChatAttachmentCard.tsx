import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { appToast } from "../../toasts";
import { openArtifactFile, type ArtifactFile } from "../artifacts/artifactFileOperations";
import type { ArtifactPreviewModel } from "../artifacts/types";
import { AttachmentIcon } from "./AttachmentIcon";
import { ChatAttachmentFilePreview } from "./ChatAttachmentFilePreview";
import { ChatAttachmentImagePreview } from "./ChatAttachmentImagePreview";
import { attachmentPresentation, attachmentPreviewModel, imageAttachmentType, readAttachmentBytes, readAttachmentImage, type AttachmentScope } from "./attachmentPresentation";
import type { ChatAttachment } from "./types";
import "./attachments.css";

function attachmentIdentity(attachment: ChatAttachment, scope?: AttachmentScope) {
  const admitted = scope !== undefined;
  const file = admitted ? undefined : attachment.file;
  const stageId = admitted || file ? undefined : attachment.stage?.id;
  const stageHash = admitted || file ? undefined : attachment.stage?.contentHash;
  const { artifactId, runId, interfaceId } = scope ?? {};
  const awaitingAdmission = Boolean(scope?.pending && !artifactId);
  const key = JSON.stringify([admitted, attachment.id, attachment.type, attachment.size, stageId, stageHash, artifactId, runId, interfaceId, awaitingAdmission]);
  return { key, file, stageId, stageHash, admitted, awaitingAdmission, artifactId, runId, interfaceId };
}

export function ChatAttachmentCard({ attachment, scope, onRemove, compact = false, open, onOpenChange, onPreviousImage, onNextImage }: {
  attachment: ChatAttachment;
  scope?: AttachmentScope;
  onRemove?: () => void;
  compact?: boolean;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  onPreviousImage?: () => void;
  onNextImage?: () => void;
}) {
  const image = imageAttachmentType(attachment.type);
  const { id, type, size } = attachment;
  const { key, file, stageId, stageHash, admitted, awaitingAdmission, artifactId, runId, interfaceId } = attachmentIdentity(attachment, scope);
  const [preview, setPreview] = useState<{ key: string; file?: File; blob?: Blob; url: string; error: string }>({ key: "", url: "", error: "" });
  const [expanded, setExpanded] = useState(false);
  const [opening, setOpening] = useState(false);
  const [detail, setDetail] = useState<{ key: string; source?: File; file: ArtifactFile; model?: ArtifactPreviewModel; error?: string } | null>(null);
  const lifetime = useRef({ active: false });
  const openingRequest = useRef<{ active: boolean } | null>(null);

  useLayoutEffect(() => {
    const current = { active: true };
    lifetime.current = current;
    setOpening(false);
    return () => { current.active = false; };
  }, [key, file]);

  useEffect(() => {
    setExpanded(false);
    setDetail(null);
    if (!image || awaitingAdmission) return;
    let retired = false;
    let url = "";
    setPreview({ key, file, url: "", error: "" });
    void readAttachmentImage({ id, type, size, file,
      stage: stageId && stageHash ? { id: stageId, contentHash: stageHash } : undefined },
      admitted ? { artifactId, runId, interfaceId } : undefined).then((blob) => {
      if (retired) return;
      url = URL.createObjectURL(blob);
      setPreview({ key, file, blob, url, error: "" });
    }).catch((error: unknown) => {
      if (!retired) setPreview({ key, file, url: "", error: error instanceof Error ? error.message : "Image preview unavailable." });
    });
    return () => {
      retired = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [key, image, id, type, size, stageId, stageHash, file, admitted, artifactId, runId, interfaceId, awaitingAdmission]);

  const current = image && preview.key === key && preview.file === file;
  const url = current ? preview.url : "";
  const error = current ? preview.error : "";
  const presentation = attachmentPresentation(attachment);
  const setOpen = onOpenChange ?? setExpanded;
  const failImage = () => setPreview((previous) => previous.key === key && previous.file === file
    ? { ...previous, url: "", error: "Image preview could not be decoded." } : previous);
  const remove = onRemove ? <button aria-label={`Remove ${attachment.name}`} className="chat-attachment-remove" type="button"
    onClick={(event) => { event.preventDefault(); event.stopPropagation(); onRemove(); }}
    onPointerDown={(event) => { event.preventDefault(); event.stopPropagation(); }}><AttachmentIcon name="close" size={12} /></button> : null;

  async function openFile() {
    const request = lifetime.current;
    if (!request.active || openingRequest.current === request) return;
    openingRequest.current = request;
    setOpening(true);
    try {
      const blob = await readAttachmentBytes({ id, type, size, file, stage: stageId && stageHash ? { id: stageId, contentHash: stageHash } : undefined },
        admitted ? { artifactId, runId, interfaceId } : undefined);
      if (!request.active) return;
      const resolved: ArtifactFile = { bytes: new Uint8Array(await blob.arrayBuffer()), filename: attachment.name, mediaType: type };
      if (!request.active) return;
      let model: ArtifactPreviewModel | null;
      try {
        model = attachmentPreviewModel(attachment, resolved.bytes, scope);
      } catch (failure: unknown) {
        if (request.active) setDetail({ key, source: file, file: resolved, error: failure instanceof Error ? failure.message : "This preview could not be rendered." });
        return;
      }
      if (!request.active) return;
      if (model) setDetail({ key, source: file, file: resolved, model });
      else await openArtifactFile(resolved, "open");
    } catch (failure: unknown) {
      if (request.active) appToast.danger(failure instanceof Error ? failure.message : "The attachment could not be opened.");
    } finally {
      if (openingRequest.current === request) openingRequest.current = null;
      if (request.active) setOpening(false);
    }
  }

  const canReadFile = !awaitingAdmission && (admitted ? Boolean(artifactId && runId && interfaceId) : Boolean(file || stageId && stageHash));
  return <div className={`chat-attachment-card ${onRemove ? "is-composer" : "is-message"}${image ? ` is-image${compact ? " is-compact" : ""}` : onRemove ? " is-file-card" : " is-file-pill"}`}
    data-preview-state={image ? url ? "ready" : error ? "failed" : "loading" : undefined} title={`${attachment.name}${error ? `\n${error}` : ""}`}>
    {image ? url ? <button aria-label={`View ${attachment.name}`} className="chat-attachment-image-button composer-attachment-surface" onClick={() => setOpen(true)} type="button"
      onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); setOpen(true); } }}>
      <img alt={attachment.name} draggable={false} onError={failImage} referrerPolicy="no-referrer" src={url} />
    </button> : <div aria-label={error ? "Image failed to load" : `${attachment.name}: ${awaitingAdmission ? "Preparing image" : "Loading image"}`}
      className="chat-attachment-image-placeholder composer-attachment-surface" role={error ? "img" : "status"}>
      {error ? <span>Failed</span> : <AttachmentIcon className="chat-attachment-loading-spinner" name="spinner" size={onRemove ? 24 : 16} />}
    </div> : <button aria-busy={opening || undefined} aria-label={attachment.name} className="chat-attachment-file-button composer-attachment-surface"
      data-composer-attachment-pill={onRemove ? undefined : "true"} onClick={canReadFile ? () => { void openFile(); } : undefined} tabIndex={canReadFile ? undefined : -1} type="button">
      <span className="chat-attachment-file-icon"><AttachmentIcon name={presentation.icon} size={onRemove ? 24 : 14} /></span>
      <span className="chat-attachment-file-copy"><strong>{attachment.name}</strong>
        {onRemove && presentation.extension ? <small>{presentation.extension}</small> : null}
      </span>
    </button>}
    {remove}
    {url && preview.blob && (open ?? expanded) ? <ChatAttachmentImagePreview blob={preview.blob} filename={attachment.name} key={url} onClose={() => setOpen(false)} onError={failImage}
      onNextImage={onNextImage} onPreviousImage={onPreviousImage} src={url} /> : null}
    {detail?.key === key && detail.source === file ? <ChatAttachmentFilePreview error={detail.error} file={detail.file} model={detail.model} onClose={() => setDetail(null)} /> : null}
  </div>;
}

export function ChatAttachmentStrip({ attachments, scope, onRemove, className = "" }: {
  attachments: ChatAttachment[]; scope?: AttachmentScope; onRemove?: (id: string) => void; className?: string;
}) {
  const [selected, setSelected] = useState<{ id: string; key: string; file?: File } | null>(null);
  const images = attachments.filter((attachment) => imageAttachmentType(attachment.type));
  const files = attachments.filter((attachment) => !imageAttachmentType(attachment.type));
  const index = images.findIndex((image) => {
    const identity = attachmentIdentity(image, scope);
    return selected?.id === image.id && selected.key === identity.key && selected.file === identity.file;
  });
  const select = (image: ChatAttachment) => {
    const { key, file } = attachmentIdentity(image, scope);
    setSelected({ id: image.id, key, file });
  };
  const imageCards = images.map((image, position) => <ChatAttachmentCard attachment={image} compact={Boolean(onRemove && files.length)} key={image.id}
    onNextImage={position < images.length - 1 ? () => select(images[position + 1]) : undefined}
    onPreviousImage={position > 0 ? () => select(images[position - 1]) : undefined}
    onOpenChange={(open) => { if (open) select(image); else setSelected(null); }} open={index === position}
    onRemove={onRemove ? () => onRemove(image.id) : undefined} scope={scope} />);
  const fileCards = files.map((file) => <ChatAttachmentCard attachment={file} key={file.id} onRemove={onRemove ? () => onRemove(file.id) : undefined} scope={scope} />);
  const row = (cards: ReactNode, kind: string) => <div className={`chat-attachment-scroll is-${kind}`}><div className="chat-attachment-row">{cards}</div></div>;
  if (!attachments.length) return null;
  return <div className={`chat-attachment-strip ${onRemove ? "is-composer" : "is-message"} ${className}`}>
    {onRemove ? row(<>{imageCards}{fileCards}</>, "composer") : <>{images.length ? row(imageCards, "images") : null}{files.length ? row(fileCards, "files") : null}</>}
  </div>;
}
