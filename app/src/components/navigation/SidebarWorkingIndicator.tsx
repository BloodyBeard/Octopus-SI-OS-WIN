import { SidebarTaskStatus } from "./SidebarTaskStatus";

export function SidebarWorkingIndicator({ label = "Running", count }: { label?: string; count?: number }) {
  return <SidebarTaskStatus state="running" label={label} text={count === undefined ? undefined : String(count)} />;
}
