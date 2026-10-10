import { DndContext, KeyboardSensor, PointerSensor, closestCenter, useSensor, useSensors, type DragEndEvent, type Modifier } from "@dnd-kit/core";
import { SortableContext, sortableKeyboardCoordinates, useSortable, verticalListSortingStrategy } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { AnimatePresence, motion, useIsPresent } from "motion/react";
import { useEffect, useRef, useState } from "react";
import { useShouldReduceMotion } from "../../appearance";
import { OcMenuItem } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { Glyph } from "../IconGlyphs";
import { MessageContent } from "./MessageContent";
import { MessageTooltip } from "./MessageTooltip";
import { markdownToPlainText } from "./markdownPresentation";
import type { ChatQueuedTurnView } from "./types";
import "./queue.css";

type Props = {
  messages: ChatQueuedTurnView[];
  notice?: string;
  paused: boolean;
  sendNowAvailable: boolean;
  onDelete: (id: string) => void;
  onEdit: (id: string) => void;
  onSendNow: (id: string) => void;
  onReorder: (activeId: string, overId: string) => void;
  onResume: () => void;
  onRetry: (id: string) => void;
};

const constrainQueueDrag: Modifier = ({ transform, draggingNodeRect, scrollableAncestorRects }) => {
  const bounds = scrollableAncestorRects[0];
  const y = !draggingNodeRect || !bounds ? transform.y
    : Math.max(bounds.top - draggingNodeRect.top, Math.min(transform.y, bounds.bottom - draggingNodeRect.bottom));
  return { ...transform, x: 0, y };
};
const queueDragModifiers = [constrainQueueDrag];
const railExit = { gridTemplateRows: "0fr", borderTopWidth: 0, borderBottomWidth: 0 };
const railTransition = { duration: 0.3, ease: [0.19, 1, 0.22, 1] as [number, number, number, number] };

export function QueuedMessageList(props: Props) {
  return <AnimatePresence initial={false}>
    {props.messages.length || props.notice ? <QueuedMessageRail {...props} key="queue" /> : null}
  </AnimatePresence>;
}

function QueuedMessageRail(props: Props) {
  const present = useIsPresent();
  const reducedMotion = useShouldReduceMotion();
  const transition = reducedMotion ? { duration: 0 } : railTransition;
  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 6 } }), useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }));
  const blocked = props.messages.some((message) => message.status === "unknown" || message.status === "sending");
  const reorderable = props.messages.filter((message) => message.status === "queued" || message.status === "failed").length > 1;
  function finishDrag(event: DragEndEvent) {
    if (present && event.over && event.active.id !== event.over.id) props.onReorder(String(event.active.id), String(event.over.id));
  }
  return (
    <motion.div className="composer-queue-surface" aria-label="Queued messages"
      aria-hidden={!present || undefined} inert={!present || undefined} data-composer-rail-item={present ? "present" : "exiting"} data-reduced-motion={reducedMotion}
      initial={reducedMotion ? false : railExit} animate={{ gridTemplateRows: "1fr", borderTopWidth: 1, borderBottomWidth: 0 }}
      exit={railExit} transition={transition}>
      <motion.div className="composer-queue-content" initial={reducedMotion ? false : { opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} transition={transition}>
      {props.notice ? <p className="composer-queue-notice" role="status">{props.notice}</p> : null}
      <div className="composer-queue-scroll vertical-scroll-fade-mask hide-scrollbar">
        {props.paused && props.messages.length ? <div className="composer-queue-paused composer-queue-row">
          <div className="composer-queue-body"><Glyph className="composer-queue-leading icon-2xs" name="pause" size={16} />
            <span className="composer-queue-title">{blocked ? "Queue paused while delivery is unconfirmed" : "Queue paused"}</span></div>
          <button className="composer-queue-action" disabled={!present || blocked} onClick={props.onResume} type="button">
            <Glyph className="icon-2xs" name="play" size={16} /><span>Resume</span>
          </button>
        </div> : null}
        <DndContext collisionDetection={closestCenter} modifiers={queueDragModifiers} onDragEnd={finishDrag} sensors={sensors}>
          <SortableContext items={props.messages.map((message) => message.id)} strategy={verticalListSortingStrategy}>
            <AnimatePresence initial={false} propagate>
              {present ? props.messages.map((message) => <QueuedMessageRow {...props} key={message.id} message={message} reorderable={reorderable} />) : null}
            </AnimatePresence>
          </SortableContext>
        </DndContext>
      </div>
      </motion.div>
    </motion.div>
  );
}

function QueuedMessageRow({ message, reorderable, sendNowAvailable, onDelete, onEdit, onRetry, onSendNow }: Props & { message: ChatQueuedTurnView; reorderable: boolean }) {
  const present = useIsPresent();
  const reducedMotion = useShouldReduceMotion();
  const mutable = message.status === "queued" || message.status === "failed";
  const canReorder = present && mutable && reorderable;
  const { attributes, isDragging, listeners, setActivatorNodeRef, setNodeRef, transform, transition } = useSortable({ id: message.id, disabled: !canReorder });
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);
  const [preview, setPreview] = useState<{ file: File; src: string } | null>(null);
  const imageFile = message.attachments.find((item) => item.type.startsWith("image/") && item.file)?.file;
  useEffect(() => {
    if (!imageFile) return;
    const src = URL.createObjectURL(imageFile);
    setPreview({ file: imageFile, src });
    return () => URL.revokeObjectURL(src);
  }, [imageFile]);
  const previewSrc = preview?.file === imageFile ? preview?.src : undefined;
  const failed = message.status === "failed";
  const unknown = message.status === "unknown";
  const editing = message.status === "editing";
  const attachmentLabel = message.attachments.map((item) => item.name).join(", ");
  const imageCount = message.attachments.filter((item) => item.type.startsWith("image/")).length;
  const attachmentSummary = [imageCount ? `${imageCount} ${imageCount === 1 ? "image" : "images"}` : "",
    ...message.attachments.filter((item) => !item.type.startsWith("image/")).map((item) => item.name)].filter(Boolean).join(", ");
  const fenced = /^(?:`{3,}|~{3,})/.test(message.text.trim());
  const failureLabel = unknown ? "Delivery unconfirmed. Check task history; this message will not be retried automatically."
    : "This queued message could not be sent. Retry, edit, or delete it to continue the queue.";
  const statusLabel = unknown ? "Delivery unconfirmed · no automatic retry" : failed ? "Not sent" : undefined;
  return (
    <motion.div animate={{ height: "auto", opacity: 1 }} className="composer-queue-motion"
      aria-hidden={!present || undefined} inert={!present || undefined}
      exit={{ height: 0, opacity: 0 }} initial={{ height: 0, opacity: 0 }} ref={setNodeRef}
      style={{ transform: CSS.Translate.toString(transform), transition: reducedMotion ? "none" : transition }} transition={{ duration: reducedMotion ? 0 : 0.18 }}>
      <div className="composer-queue-row group" data-message-status={message.status} data-muted={isDragging || editing || undefined} aria-busy={message.status === "sending"}>
        <div className="composer-queue-body">
          {canReorder ? <span {...attributes} {...listeners} aria-label="Reorder queued message" className="composer-queue-drag" ref={setActivatorNodeRef}>
            <Glyph className="composer-queue-drag-indicator icon-2xs" name="drag" size={16} />
            <Glyph className="composer-queue-leading icon-2xs" name="steer" size={16} />
          </span> : <Glyph className="composer-queue-leading icon-2xs" name="steer" size={16} />}
          <div className="composer-queue-copy">
            <div className="composer-queue-title" title={[message.text, attachmentLabel].filter(Boolean).join("\n")}>
              {failed || unknown ? <MessageTooltip label={[failureLabel, message.error].filter(Boolean).join("\n")}>
                <span className="composer-queue-warning" aria-label={failureLabel}><Glyph className="icon-2xs" name="warning" size={16} /></span>
              </MessageTooltip> : null}
              {previewSrc ? <img className="composer-queue-image" src={previewSrc} alt="Image attachment" draggable={false} /> : null}
              <div className="composer-queue-message">{!message.text.trim() ? attachmentSummary : fenced
                ? markdownToPlainText(message.text).replace(/\s+/g, " ").trim()
                : <MessageContent compactHeadings text={message.text} variant="user" />}</div>
            </div>
            {statusLabel ? <span className="composer-queue-status" role="status">{statusLabel}</span> : null}
            {message.error ? <span className="composer-queue-error">{message.error}</span> : null}
          </div>
        </div>
        <div className="composer-queue-actions">
          {mutable ? <MessageTooltip label={failed ? "Try sending this queued message again. Edit or delete it if retry keeps failing." : "Submit without interrupting the model"}>
            <button aria-label={failed ? "Retry queued message" : "Steer with queued message"} className="composer-queue-action"
              disabled={!present || !sendNowAvailable} onClick={() => failed ? onRetry(message.id) : onSendNow(message.id)} type="button">
              <Glyph className="icon-2xs" name="sendNow" size={16} /><span>{failed ? "Retry" : "Steer"}</span>
            </button>
          </MessageTooltip> : null}
          {mutable || unknown ? <MessageTooltip label={unknown ? "Remove locally; this does not cancel or resend the message" : "Delete queued message"}>
            <button aria-label={unknown ? "Remove unconfirmed message locally" : "Delete queued message"} className="composer-queue-action is-icon" disabled={!present} onClick={() => onDelete(message.id)} type="button"><Glyph className="icon-2xs" name="close" size={16} /></button>
          </MessageTooltip> : null}
          {editing ? <button className="composer-queue-action" disabled={!present} onClick={() => onEdit(message.id)} type="button">Editing</button> : null}
          {mutable ? <>
            <button aria-label="Queued message actions" aria-expanded={present && menuOpen} aria-haspopup="menu" className="composer-queue-action is-icon" disabled={!present} ref={menuRef} onClick={() => setMenuOpen((open) => !open)} type="button"><Glyph className="icon-2xs" name="more" size={16} /></button>
            {present ? <OcDropdown anchorRef={menuRef} align="end" open={menuOpen} onOpenChange={setMenuOpen}>
              <OcMenuItem role="menuitem" className="composer-queue-menu-item" label="Edit message" leadingIcon="edit" onClick={() => { setMenuOpen(false); onEdit(message.id); }} />
            </OcDropdown> : null}
          </> : null}
        </div>
      </div>
    </motion.div>
  );
}
