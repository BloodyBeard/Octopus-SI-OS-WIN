import { createElement, useId, type ReactNode } from "react";
import icons from "./attachmentIcons.json";

export type AttachmentIconName = keyof typeof icons;
type IconNode = { tag: string; props: Record<string, string | number>; children?: IconNode[] };

export function AttachmentIcon({ name, size, className }: { name: AttachmentIconName; size: number; className?: string }) {
  const prefix = useId();
  const render = (node: IconNode, index: number): ReactNode => createElement(node.tag,
    Object.fromEntries(Object.entries(node.props).map(([key, value]) => [key, key === "id" ? `${prefix}-${value}`
      : typeof value === "string" && value.startsWith("url(#") ? value.replace("url(#", `url(#${prefix}-`) : value]).concat([["key", index]])),
    node.children?.map(render));
  const icon = icons[name] as IconNode;
  return <svg aria-hidden="true" className={className} fill="none" height={size} viewBox={String(icon.props.viewBox)} width={size}>
    {icon.children?.map(render)}
  </svg>;
}
