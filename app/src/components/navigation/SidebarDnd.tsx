import {
  DndContext,
  DragOverlay,
  KeyboardSensor,
  PointerSensor,
  closestCenter,
  pointerWithin,
  useDroppable,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
import {
  SortableContext,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

export type SidebarDragEntity = "surface" | "chat" | "project";

export type SidebarDragData = {
  entity: SidebarDragEntity;
  itemId: string;
  containerId: string;
};

export type SidebarDropData = {
  containerId: string;
};

type SortableHookResult = ReturnType<typeof useSortable>;

export type SidebarDragHandle = {
  listeners: SortableHookResult["listeners"];
  setActivatorNodeRef: SortableHookResult["setActivatorNodeRef"];
};

const collisionDetection: CollisionDetection = (input) => {
  const pointerCollisions = pointerWithin(input);
  return pointerCollisions.length > 0 ? pointerCollisions : closestCenter(input);
};

export function SidebarDndContext({
  children,
  onDragCancel,
  onDragEnd,
  onDragStart,
  renderDragOverlay,
}: {
  children: ReactNode;
  onDragCancel?: () => void;
  onDragEnd: (event: DragEndEvent) => void;
  onDragStart?: (event: DragStartEvent) => void;
  renderDragOverlay?: (data: SidebarDragData) => ReactNode;
}) {
  const [activeData, setActiveData] = useState<SidebarDragData | null>(null);
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  return (
    <DndContext
      collisionDetection={collisionDetection}
      onDragCancel={() => {
        setActiveData(null);
        onDragCancel?.();
      }}
      onDragEnd={(event) => {
        onDragEnd(event);
        setActiveData(null);
      }}
      onDragStart={(event) => {
        setActiveData(readSidebarDragData(event.active.data.current));
        onDragStart?.(event);
      }}
      sensors={sensors}
    >
      {children}
      {createPortal(
        <DragOverlay adjustScale={false} dropAnimation={null}>
          {activeData && renderDragOverlay ? (
            <div className="sidebar-drag-overlay">{renderDragOverlay(activeData)}</div>
          ) : null}
        </DragOverlay>,
        document.body,
      )}
    </DndContext>
  );
}

export function SidebarSortableList({ children, items }: { children: ReactNode; items: string[] }) {
  return (
    <SortableContext items={items} strategy={verticalListSortingStrategy}>
      {children}
    </SortableContext>
  );
}

export function SidebarSortableItem({
  children,
  data,
  id,
}: {
  children: ReactNode | ((dragHandle: SidebarDragHandle) => ReactNode);
  data: SidebarDragData;
  id: string;
}) {
  const {
    attributes,
    isDragging,
    isOver,
    listeners,
    setActivatorNodeRef,
    setNodeRef,
    transform,
    transition,
  } = useSortable({ id, data });
  const usesHandle = typeof children === "function";
  return (
    <div
      {...attributes}
      {...(usesHandle ? {} : listeners)}
      className="sidebar-sortable-item"
      data-sidebar-dragging={isDragging || undefined}
      data-sidebar-over={isOver && !isDragging || undefined}
      ref={setNodeRef}
      style={{
        position: "relative",
        touchAction: "none",
        transform: CSS.Transform.toString(isDragging ? null : transform),
        transition,
        zIndex: isDragging ? 3 : undefined,
      }}
    >
      {usesHandle ? children({ listeners, setActivatorNodeRef }) : children}
    </div>
  );
}

export function SidebarDropContainer({
  children,
  className = "",
  containerId,
  id,
}: {
  children: ReactNode;
  className?: string;
  containerId: string;
  id: string;
}) {
  const { isOver, setNodeRef } = useDroppable({ id, data: { containerId } satisfies SidebarDropData });
  return (
    <div
      className={`${className}${isOver ? " sidebar-drop-container-over" : ""}`}
      data-sidebar-drop-over={isOver || undefined}
      ref={setNodeRef}
    >
      {children}
    </div>
  );
}

export function sidebarItemDndId(entity: SidebarDragEntity, itemId: string): string {
  return `sidebar:${entity}:${itemId}`;
}

function readSidebarDragData(value: unknown): SidebarDragData | null {
  if (!value || typeof value !== "object") return null;
  const data = value as Partial<SidebarDragData>;
  if (
    (data.entity !== "surface" && data.entity !== "chat" && data.entity !== "project")
    || typeof data.itemId !== "string"
    || typeof data.containerId !== "string"
  ) return null;
  return data as SidebarDragData;
}
