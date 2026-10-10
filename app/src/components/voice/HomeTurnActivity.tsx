import { CadencedShimmerText } from "../CadencedShimmerText";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { MessageContent } from "../chat/MessageContent";
import { TurnActivity } from "../chat/TurnActivity";
import { conversationTraceAdmission } from "../chat/conversationTrace";
import { dynamicToolGlyph, dynamicToolOperationLabel, dynamicToolPresentation, dynamicToolReadLabel } from "../chat/toolPresentation";
import type { ChatRenderMessage, ChatToolPresentationIndex } from "../chat/types";

type HomeLiveAction = { glyph: GlyphName | null; id: string; label: string };

export function homeTurnActivity(messages: ChatRenderMessage[], promptCreatedAtMs: number): ChatRenderMessage[] {
  if (promptCreatedAtMs <= 0) return [];
  return messages.filter((message) => (
    message.role !== "user"
    && message.createdAtMs >= promptCreatedAtMs
    && !["message", "message_delta", "error", "approval", "user_input", "elicitation"].includes(message.eventKind ?? "message")
    && message.eventData?._movement_evidence !== true
  ));
}

export function HomeTurnActivity({ activities, startedAtMs, toolPresentations }: {
  activities: ChatRenderMessage[];
  startedAtMs: number;
  toolPresentations: ChatToolPresentationIndex;
}) {
  const hasActivity = activities.some((message) => conversationTraceAdmission(message, toolPresentations) === "visible");
  if (!hasActivity) return <span role="status"><CadencedShimmerText active>Octopus is working</CadencedShimmerText></span>;
  return (
    <section aria-label="Current run activity" className="home-live-activity">
      <TurnActivity
        activities={activities}
        activityWindow="latest-context"
        presentation="conversation"
        renderActivity={(message) => <HomeActivityDetail message={message} toolPresentations={toolPresentations} />}
        startedAtMs={startedAtMs}
        statusBoundaryVisible={false}
        toolPresentations={toolPresentations}
        turnInProgress
      />
    </section>
  );
}

function HomeActivityDetail({ message, toolPresentations }: {
  message: ChatRenderMessage;
  toolPresentations: ChatToolPresentationIndex;
}) {
  const kind = message.eventKind ?? "message";
  if (["commentary", "commentary_delta", "reasoning", "reasoning_delta", "plan", "plan_delta"].includes(kind)) {
    return message.text ? <MessageContent text={message.text} streaming={kind.endsWith("_delta")} streamKey={message.id} variant={kind.startsWith("reasoning") ? "reasoning" : "assistant"} /> : null;
  }
  const actions = projectHomeLiveActions(message, toolPresentations);
  if (!actions.length) return null;
  return (
    <ul className="home-live-activity__actions">
      {actions.map((action) => (
        <li key={action.id}>
          {action.glyph ? <Glyph name={action.glyph} size={13} /> : null}
          <CadencedShimmerText active={homeActivityIsActive(message)}>{action.label}</CadencedShimmerText>
        </li>
      ))}
    </ul>
  );
}

function homeActivityIsActive(entry: ChatRenderMessage): boolean {
  if ("pending" in entry && entry.pending) return true;
  const data = homeEventRecord(entry.eventData);
  const status = homeEventText(data.status, data.state, data.lifecycle, homeEventRecord(data._event).lifecycle)
    .toLocaleLowerCase();
  if (["started", "starting", "running", "working", "in_progress", "pending", "streaming", "progress"].includes(status)) return true;
  if (["completed", "complete", "done", "failed", "cancelled", "canceled", "success", "succeeded", "exited"].includes(status)) return false;
  return ["tool_output_delta", "file_change_delta", "mcp_progress", "sleep"].includes(entry.eventKind ?? "");
}

function projectHomeLiveActions(
  entry: ChatRenderMessage,
  toolPresentations: ChatToolPresentationIndex,
): HomeLiveAction[] {
  const kind = entry.eventKind ?? "message";
  const data = homeEventRecord(entry.eventData);
  const identity = homeActivityIdentity(entry) || entry.id;
  if (kind === "file_change") {
    const changes = homeEventArray(data.changes).map(homeEventRecord);
    if (changes.length) return changes.map((change, index) => {
      const path = homeEventText(change.path, change.file, change.name) || `File ${index + 1}`;
      const status = homeEventText(change.kind, change.type, change.status);
      return {
        glyph: "file",
        id: `${identity}:file:${path}`,
        label: `${homeFileAction(status)} ${path}`,
      };
    });
    const path = homeEventText(data.path, data.file);
    return [{ glyph: "file", id: `${identity}:file`, label: path ? `Updated ${path}` : homeActivityLine(entry.text) || "Updated files" }];
  }
  if (kind === "file_change_delta") return [{ glyph: "file", id: `${identity}:file-delta`, label: homeActivityLine(entry.text) || "Updating files" }];
  if (kind === "diff") return [{ glyph: "changes", id: `${identity}:diff`, label: "Updated the working tree diff" }];
  if (kind === "tool" || kind === "tool_output_delta") {
    const summary = homeEventText(data.summary, data.action) || homeActivityLine(entry.text);
    const command = homeEventText(data.command, data.tool);
    const status = homeEventText(data.status).toLocaleLowerCase();
    const active = status === "started" || status === "running" || status === "in_progress";
    const label = summary || (command ? `${active ? "Running" : "Ran"} ${command}` : active ? "Running a tool" : "Completed a tool action");
    return [{ glyph: "terminal", id: `${identity}:tool`, label }];
  }
  if (kind === "web_search") {
    const query = homeEventText(data.query) || homeActivityLine(entry.text);
    return [{ glyph: "search", id: `${identity}:search`, label: query ? `Searched for ${query}` : "Searched the web" }];
  }
  if (kind === "model_verification") return [{ glyph: "check", id: `${identity}:verification`, label: homeActivityLine(entry.text) || "Verified the projected result" }];
  if (kind === "auth_recovery_started") return [{ glyph: "pulse", id: `${identity}:auth-recovery`, label: homeActivityLine(entry.text) || "Recovering authentication" }];
  if (kind === "auth_recovery_completed") return [{ glyph: "check", id: `${identity}:auth-recovery`, label: homeActivityLine(entry.text) || "Authentication recovered" }];
  if (kind === "dynamic_tool") {
    const presentation = dynamicToolPresentation(entry as ChatRenderMessage, toolPresentations);
    const active = homeActivityIsActive(entry);
    if (presentation.family === "read") {
      return [{ glyph: presentation.kind === "search" ? "search" : presentation.kind === "list" ? "folder" : "read", id: `${identity}:dynamic`, label: dynamicToolReadLabel(presentation, active) }];
    }
    if (presentation.family === "files") {
      return [{ glyph: "edit", id: `${identity}:dynamic`, label: [active ? "Editing" : "Edited", presentation.target || "files"].join(" ") }];
    }
    if (presentation.family === "exec") {
      return [{ glyph: "terminal", id: `${identity}:dynamic`, label: [active ? "Running" : "Ran", presentation.target || "command"].join(" ") }];
    }
    return [{ glyph: dynamicToolGlyph(presentation), id: `${identity}:dynamic`, label: dynamicToolOperationLabel(presentation) }];
  }
  if (kind === "mcp_tool" || kind === "mcp_progress" || kind === "mcp_status") {
    const target = [homeEventText(data.server), homeEventText(data.tool, data.name)].filter(Boolean).join(" · ");
    return [{ glyph: "plugins", id: `${identity}:mcp`, label: homeActivityLine(entry.text) || (target ? `Called ${target}` : "Completed an interface action") }];
  }
  if (kind === "collaboration" || kind === "subagent_activity") return [{ glyph: "user", id: `${identity}:collaboration`, label: homeActivityLine(entry.text) || "Coordinated run work" }];
  if (kind === "image_view" || kind === "image_generation") return [{ glyph: "image", id: `${identity}:image`, label: homeActivityLine(entry.text) || (kind === "image_generation" ? "Generated an image" : "Inspected an image") }];
  const label = homeActivityLine(entry.text) || entry.label?.trim() || "Updated run activity";
  return [{ glyph: "pulse", id: `${identity}:${kind}`, label }];
}

function homeActivityIdentity(entry: ChatRenderMessage): string {
  const data = homeEventRecord(entry.eventData);
  const event = homeEventRecord(data._event);
  return homeEventText(data.item_id, data.itemId, data.call_id, data.callId, data.process_id, data.processId, event.item_id);
}

function homeActivityLine(value: string): string {
  const line = value.split(/\r?\n/).map((entry) => entry.trim()).find(Boolean) ?? "";
  const normalized = line
    .replace(/^#{1,6}\s+/, "")
    .replace(/^[-*+]\s+/, "")
    .replace(/^\d+[.)]\s+/, "")
    .trim();
  return normalized.length > 180 ? `${normalized.slice(0, 177)}…` : normalized;
}

function homeFileAction(status: string): string {
  const value = status.toLocaleLowerCase();
  if (value.includes("add") || value.includes("creat") || value.includes("new")) return "Created";
  if (value.includes("delete") || value.includes("remove")) return "Deleted";
  if (value.includes("rename") || value.includes("move")) return "Renamed";
  return "Updated";
}

function homeEventRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function homeEventArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function homeEventText(...values: unknown[]): string {
  return values.find((value): value is string => typeof value === "string" && Boolean(value.trim()))?.trim() ?? "";
}
