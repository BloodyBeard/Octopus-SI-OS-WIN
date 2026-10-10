import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Glyph } from "../IconGlyphs";
import { CadencedShimmerText } from "../CadencedShimmerText";
import { OcButton, OcDialog, OcIconButton } from "../../design-system/components";
import { MCP_RESOURCE_ACTION_EVENT, type McpResourceActionDetail } from "../../clientEvents";
import { copyText } from "../../runtimeFormat";
import type { ChatRenderMessage } from "./types";
import "./mcp-resource.css";

type Props = {
  message: ChatRenderMessage;
};

type HtmlResource = {
  html: string;
  mimeType: string;
  uri: string;
};

type ResourceDescriptor = {
  annotations: Record<string, unknown>;
  blob: string;
  description: string;
  mimeType: string;
  name: string;
  size: number;
  text: string;
  title: string;
  uri: string;
};

type ActionPayload = {
  ok: boolean;
  value: unknown;
};

type StructuredAction = {
  action: string;
  disabled: boolean;
  label: string;
  payload: unknown;
  tone: "neutral" | "danger";
};

const MAX_COLLECTION_ITEMS = 48;
const MAX_MEDIA_DATA_LENGTH = 12_000_000;
const MAX_ACTION_PAYLOAD_LENGTH = 128_000;

export function McpResourceSurface({ message }: Props) {
  const rootRef = useRef<HTMLDivElement>(null);
  const data = record(message.eventData);
  const result = record(data.result);
  const appContext = record(data.appContext ?? data.app_context);
  const movement = record(data._movement);
  const movementMetadata = record(movement.metadata);
  const runtimeMetadata = record(movementMetadata.runtime);
  const content = array(result.content ?? data.content);
  const structuredContent = firstDefined(
    result.structuredContent,
    result.structured_content,
    data.structuredContent,
    data.structured_content,
  );
  const htmlResource = extractHtmlResource(content, data, result);
  const resourceUri = firstString(
    data.mcpAppResourceUri,
    data.mcp_app_resource_uri,
    appContext.resourceUri,
    appContext.resource_uri,
    data.resourceUri,
    data.resource_uri,
    extractMetaResourceUri(record(result._meta ?? result.meta)),
    htmlResource?.uri,
  );
  const server = firstString(data.server, record(data.invocation).server);
  const tool = firstString(data.tool, record(data.invocation).tool);
  const status = firstString(data.status, data.state, record(data._event).lifecycle);
  const durationMs = number(data.durationMs ?? data.duration_ms ?? message.latencyMs);
  const errorMessage = extractError(data, result, status);
  const active = isActiveStatus(status) || message.eventKind === "mcp_progress" || message.pending === true;
  const rich = Boolean(resourceUri || htmlResource || structuredContent !== undefined || content.length || errorMessage);
  const [expanded, setExpanded] = useState(rich);
  const [fullView, setFullView] = useState(false);
  const [showRaw, setShowRaw] = useState(false);
  const provenance = useMemo(() => ({
    artifactId: message.artifactId ?? "",
    interfaceId: firstString(message.interfaceId, runtimeMetadata.interface, endpointInterface(movement.to), endpointInterface(movement.from)),
    messageId: message.id,
    movementSeq: number(movement.seq),
    resourceUri,
    runId: firstString(message.runId, runtimeMetadata.run),
    server,
    tool,
    traceId: firstString(movement.trace_id, movement.traceId, runtimeMetadata.trace_id, runtimeMetadata.traceId),
  }), [htmlResource, message.artifactId, message.id, message.interfaceId, message.runId, movement, resourceUri, runtimeMetadata, server, tool]);
  const actionsAvailable = Boolean(provenance.interfaceId && provenance.resourceUri && provenance.runId);

  useEffect(() => {
    if (rich) setExpanded(true);
  }, [rich]);



  const emitWidgetAction = useCallback((action: string, payload: unknown) => {
    const root = rootRef.current;
    if (!root || !actionsAvailable || !validActionName(action)) return;
    const normalized = normalizeActionPayload(payload);
    if (!normalized.ok) return;
    const detail: McpResourceActionDetail = {
      schemaVersion: 1,
      action,
      payload: normalized.value,
      provenance,
    };
    root.dispatchEvent(new CustomEvent<McpResourceActionDetail>(MCP_RESOURCE_ACTION_EVENT, {
      bubbles: true,
      cancelable: true,
      composed: true,
      detail,
    }));
  }, [actionsAvailable, provenance]);

  const title = [server, tool].filter(Boolean).join(" · ") || message.text || "MCP";
  const renderBody = (full: boolean) => (
    <McpResourceBody
      actionsAvailable={actionsAvailable}
      active={active}
      content={content}
      data={data}
      errorMessage={errorMessage}
      full={full}
      htmlResource={htmlResource}
      onCopyResource={() => void copyLocalText(resourceUri)}
      onWidgetAction={emitWidgetAction}
      resourceUri={resourceUri}
      server={server}
      showRaw={showRaw}
      structuredContent={structuredContent}
      tool={tool}
    />
  );

  return (
    <div
      className={`mcp-resource-surface${errorMessage ? " is-error" : ""}`}
      data-state={active ? "active" : errorMessage ? "error" : "complete"}
      ref={rootRef}
    >
      <button
        aria-expanded={expanded}
        className="mcp-resource-summary"
        onClick={() => setExpanded((current) => !current)}
        type="button"
      >
        <span className="mcp-resource-summary-icon">
          {active ? <span className="composer-spinner size-3.5" /> : <Glyph name={errorMessage ? "warning" : "plugins"} size={14} />}
        </span>
        <span className="mcp-resource-summary-copy">
          <strong><CadencedShimmerText active={active}>{title}</CadencedShimmerText></strong>
          <small>
            {errorMessage
              ? "Failed"
              : resourceUri
                ? htmlResource
                  ? "Interactive resource"
                  : "Resource reference"
                : active
                  ? "Calling"
                  : humanize(status || "Completed")}
          </small>
        </span>
        {durationMs > 0 ? <span className="mcp-resource-duration">{formatDuration(durationMs)}</span> : null}
        <Glyph className={`mcp-resource-chevron${expanded ? " is-open" : ""}`} name="chevronDown" size={12} />
      </button>

      {expanded ? (
        <div className="mcp-resource-inline">
          <div className="mcp-resource-toolbar">
            <span>{resourceUri ? "MCP app output" : "MCP tool output"}</span>
            <span className="mcp-resource-toolbar-actions">
              <OcButton
                onClick={() => setShowRaw((current) => !current)}
                selected={showRaw}
                size="xs"
                variant="ghost"
              >
                Raw
              </OcButton>
              {rich ? (
                <OcIconButton
                  glyph="expand"
                  label="Open MCP output in full view"
                  onClick={() => setFullView(true)}
                  size="xs"
                  variant="ghost"
                />
              ) : null}
            </span>
          </div>
          {fullView ? <div className="mcp-resource-inline-placeholder">Open in full view</div> : renderBody(false)}
        </div>
      ) : null}

      <OcDialog ariaLabel={`${title} MCP output`} backdropClassName="mcp-resource-modal-backdrop" onClose={() => setFullView(false)} open={fullView} panelClassName="mcp-resource-modal" size="editor">
        <header className="mcp-resource-modal-header">
          <span className="mcp-resource-summary-icon"><Glyph name="plugins" size={14} /></span>
          <span className="mcp-resource-modal-title">
            <strong>{title}</strong>
            <small>{resourceUri || "Structured tool output"}</small>
          </span>
          <OcButton
            onClick={() => setShowRaw((current) => !current)}
            selected={showRaw}
            size="xs"
            variant="ghost"
          >
            Raw
          </OcButton>
          <OcIconButton glyph="close" label="Close full view" onClick={() => setFullView(false)} size="xs" variant="ghost" />
        </header>
        <div className="mcp-resource-modal-body">{fullView ? renderBody(true) : null}</div>
      </OcDialog>
    </div>
  );
}

function McpResourceBody({
  actionsAvailable,
  active,
  content,
  data,
  errorMessage,
  full,
  htmlResource,
  onCopyResource,
  onWidgetAction,
  resourceUri,
  server,
  showRaw,
  structuredContent,
  tool,
}: {
  actionsAvailable: boolean;
  active: boolean;
  content: unknown[];
  data: Record<string, unknown>;
  errorMessage: string;
  full: boolean;
  htmlResource: HtmlResource | null;
  onCopyResource: () => void;
  onWidgetAction: (action: string, payload: unknown) => void;
  resourceUri: string;
  server: string;
  showRaw: boolean;
  structuredContent: unknown;
  tool: string;
}) {
  const visibleContent = suppressDuplicatedJsonText(content, structuredContent);
  const hasVisibleOutput = visibleContent.length > 0 || structuredContent !== undefined;
  const structuredActions = extractStructuredActions(structuredContent);
  return (
    <div className={`mcp-resource-body${full ? " is-full" : ""}`}>
      {resourceUri ? (
        <ResourceReferenceCard
          htmlAvailable={Boolean(htmlResource)}
          onCopy={onCopyResource}
          resourceUri={resourceUri}
          server={server}
          tool={tool}
        />
      ) : null}

      {errorMessage ? <McpState glyph="warning" title="MCP call failed">{errorMessage}</McpState> : null}

      {htmlResource ? (
        <McpHtmlFrame
          full={full}
          html={htmlResource.html}
          title={`${server || "MCP"} ${tool || "resource"}`}
        />
      ) : resourceUri ? (
        <McpState glyph="lock" title="Interactive resource unavailable">
          This movement contains the resource URI but not its HTML bytes. Octopus does not fetch or imitate the app surface.
        </McpState>
      ) : null}

      {visibleContent.length ? (
        <section className="mcp-resource-section">
          <h4>Content</h4>
          <div className="mcp-content-blocks">
            {visibleContent.slice(0, MAX_COLLECTION_ITEMS).map((block, index) => (
              <McpContentBlock block={block} key={`content:${index}`} />
            ))}
          </div>
          {visibleContent.length > MAX_COLLECTION_ITEMS ? <CollectionRemainder count={visibleContent.length - MAX_COLLECTION_ITEMS} /> : null}
        </section>
      ) : null}

      {structuredContent !== undefined ? (
        <section className="mcp-resource-section">
          <h4>Structured content</h4>
          <StructuredValue value={structuredContent} />
        </section>
      ) : null}

      {structuredActions.length ? (
        <section className="mcp-resource-section">
          <h4>Suggested actions</h4>
          <div className="mcp-resource-actions">
            {structuredActions.map((action) => (
              <OcButton
                disabled={!actionsAvailable || action.disabled}
                key={action.action}
                onClick={() => onWidgetAction(action.action, action.payload)}
                size="xs"
                tone={action.tone}
                variant="secondary"
              >
                {action.label}
              </OcButton>
            ))}
          </div>
        </section>
      ) : null}

      {!errorMessage && !resourceUri && !hasVisibleOutput ? (
        active
          ? <McpState glyph="pulse" title="Waiting for MCP output">The interface has not emitted result content yet.</McpState>
          : <McpState glyph="artifact" title="No result content">The MCP tool completed without displayable content.</McpState>
      ) : null}

      {htmlResource && !actionsAvailable ? (
        <div className="mcp-resource-provenance-warning">
          Widget actions are disabled because run, interface, or resource provenance is missing.
        </div>
      ) : null}

      {showRaw ? (
        <section className="mcp-resource-section">
          <h4>Raw movement payload</h4>
          <JsonBlock value={data} />
        </section>
      ) : null}
    </div>
  );
}

function ResourceReferenceCard({ htmlAvailable, onCopy, resourceUri, server, tool }: {
  htmlAvailable: boolean;
  onCopy: () => void;
  resourceUri: string;
  server: string;
  tool: string;
}) {
  return (
    <div className="mcp-resource-reference">
      <span className="mcp-resource-reference-icon"><Glyph name="plugins" size={15} /></span>
      <span className="mcp-resource-reference-copy">
        <strong>{resourceUri.startsWith("ui://") ? "MCP app resource" : "MCP resource"}</strong>
        <code title={resourceUri}>{resourceUri}</code>
        <small>{[server, tool, htmlAvailable ? "HTML delivered" : "Reference only"].filter(Boolean).join(" · ")}</small>
      </span>
      <OcIconButton glyph="copy" label="Copy resource URI" onClick={onCopy} size="xs" variant="ghost" />
    </div>
  );
}

function McpHtmlFrame({ full, html, title }: {
  full: boolean;
  html: string;
  title: string;
}) {
  const source = useMemo(() => sandboxDocument(html), [html]);

  return (
    <div className={`mcp-html-frame${full ? " is-full" : ""}`}>
      <iframe
        allow=""
        referrerPolicy="no-referrer"
        sandbox=""
        srcDoc={source}
        title={title}
      />
      <div className="mcp-html-frame-caption">
        <Glyph name="lock" size={11} />
        <span>Sandboxed · scripts, network, Electron bridge, host DOM, and automatic actions are disabled</span>
      </div>
    </div>
  );
}

function McpContentBlock({ block }: { block: unknown }) {
  const value = record(block);
  const type = text(value.type).toLocaleLowerCase();
  const annotations = record(value.annotations);
  if (type === "text") {
    return (
      <div className="mcp-content-text">
        <ContentBlockHeading annotations={annotations} label="plaintext" />
        <pre>{text(value.text)}</pre>
      </div>
    );
  }
  if (type === "image") {
    const mimeType = firstString(value.mimeType, value.mime_type);
    const encoded = text(value.data);
    if (mimeType.startsWith("image/") && encoded && encoded.length <= MAX_MEDIA_DATA_LENGTH) {
      return (
        <figure className="mcp-content-media">
          <img alt="MCP result" src={`data:${mimeType};base64,${encoded}`} />
          <AnnotationLine annotations={annotations} />
        </figure>
      );
    }
  }
  if (type === "audio") {
    const mimeType = firstString(value.mimeType, value.mime_type);
    const encoded = text(value.data);
    if (mimeType.startsWith("audio/") && encoded && encoded.length <= MAX_MEDIA_DATA_LENGTH) {
      return (
        <figure className="mcp-content-media">
          <audio controls preload="metadata" src={`data:${mimeType};base64,${encoded}`} />
          <AnnotationLine annotations={annotations} />
        </figure>
      );
    }
  }
  if (type === "resource_link" || type === "resourcelink") {
    return <ResourceDescriptorCard descriptor={resourceDescriptor(value)} linked />;
  }
  if (type === "embedded_resource" || type === "embeddedresource" || type === "resource") {
    return <ResourceDescriptorCard descriptor={resourceDescriptor(record(value.resource).uri ? record(value.resource) : value)} />;
  }
  return <JsonBlock value={block} />;
}

function ResourceDescriptorCard({ descriptor, linked = false }: { descriptor: ResourceDescriptor; linked?: boolean }) {
  const content = descriptor.text || descriptor.blob;
  return (
    <article className="mcp-embedded-resource">
      <header>
        <span><Glyph name={linked ? "openExternal" : "artifact"} size={13} /></span>
        <div>
          <strong>{descriptor.title || descriptor.name || (linked ? "Resource link" : "Embedded resource")}</strong>
          {descriptor.description ? <small>{descriptor.description}</small> : null}
        </div>
        {descriptor.uri ? <OcIconButton glyph="copy" label="Copy resource URI" onClick={() => void copyLocalText(descriptor.uri)} size="xs" variant="ghost" /> : null}
      </header>
      <dl>
        {descriptor.uri ? <div><dt>URI</dt><dd><code>{descriptor.uri}</code></dd></div> : null}
        {descriptor.mimeType ? <div><dt>MIME type</dt><dd>{descriptor.mimeType}</dd></div> : null}
        {descriptor.size > 0 ? <div><dt>Size</dt><dd>{descriptor.size.toLocaleString()} bytes</dd></div> : null}
        {Object.keys(descriptor.annotations).length ? <div><dt>Annotations</dt><dd>{annotationText(descriptor.annotations)}</dd></div> : null}
      </dl>
      {content ? (
        <pre className="mcp-embedded-resource-content">{descriptor.blob ? `[base64 blob · ${descriptor.blob.length.toLocaleString()} characters]` : content}</pre>
      ) : null}
    </article>
  );
}

function StructuredValue({ value, depth = 0 }: { value: unknown; depth?: number }) {
  if (value == null) return <span className="mcp-structured-null">null</span>;
  if (typeof value === "boolean" || typeof value === "number") return <code className="mcp-structured-scalar">{String(value)}</code>;
  if (typeof value === "string") {
    return value.includes("\n") || value.length > 160
      ? <pre className="mcp-structured-text">{value}</pre>
      : <span className="mcp-structured-string">{value}</span>;
  }
  if (depth >= 7) return <JsonBlock value={value} />;
  if (Array.isArray(value)) {
    if (isTabular(value)) return <StructuredTable rows={value} />;
    return (
      <ol className="mcp-structured-list">
        {value.slice(0, MAX_COLLECTION_ITEMS).map((entry, index) => (
          <li key={`item:${index}`}><StructuredValue depth={depth + 1} value={entry} /></li>
        ))}
        {value.length > MAX_COLLECTION_ITEMS ? <li><CollectionRemainder count={value.length - MAX_COLLECTION_ITEMS} /></li> : null}
      </ol>
    );
  }
  const entries = Object.entries(record(value));
  if (!entries.length) return <span className="mcp-structured-null">Empty object</span>;
  return (
    <dl className="mcp-structured-object">
      {entries.slice(0, MAX_COLLECTION_ITEMS).map(([name, entry]) => (
        <div key={name}>
          <dt>{humanize(name)}</dt>
          <dd><StructuredValue depth={depth + 1} value={entry} /></dd>
        </div>
      ))}
      {entries.length > MAX_COLLECTION_ITEMS ? <div><dt>More</dt><dd><CollectionRemainder count={entries.length - MAX_COLLECTION_ITEMS} /></dd></div> : null}
    </dl>
  );
}

function StructuredTable({ rows }: { rows: Array<Record<string, unknown>> }) {
  const keys = [...new Set(rows.flatMap((row) => Object.keys(row)))].slice(0, 8);
  return (
    <div className="mcp-structured-table-scroll">
      <table className="mcp-structured-table">
        <thead><tr>{keys.map((key) => <th key={key}>{humanize(key)}</th>)}</tr></thead>
        <tbody>
          {rows.slice(0, MAX_COLLECTION_ITEMS).map((row, index) => (
            <tr key={`row:${index}`}>{keys.map((key) => <td key={key}><StructuredValue value={row[key]} /></td>)}</tr>
          ))}
        </tbody>
      </table>
      {rows.length > MAX_COLLECTION_ITEMS ? <CollectionRemainder count={rows.length - MAX_COLLECTION_ITEMS} /> : null}
    </div>
  );
}

function ContentBlockHeading({ annotations, label }: { annotations: Record<string, unknown>; label: string }) {
  return (
    <div className="mcp-content-heading">
      <span>{label}</span>
      <AnnotationLine annotations={annotations} />
    </div>
  );
}

function AnnotationLine({ annotations }: { annotations: Record<string, unknown> }) {
  const value = annotationText(annotations);
  return value ? <small>{value}</small> : null;
}

function McpState({ children, glyph, title }: { children: ReactNode; glyph: Parameters<typeof Glyph>[0]["name"]; title: string }) {
  return (
    <div className="mcp-resource-state">
      <span><Glyph name={glyph} size={15} /></span>
      <div><strong>{title}</strong><p>{children}</p></div>
    </div>
  );
}

function JsonBlock({ value }: { value: unknown }) {
  return <pre className="mcp-json-block">{safeJson(value)}</pre>;
}

function CollectionRemainder({ count }: { count: number }) {
  return <div className="mcp-collection-remainder">{count.toLocaleString()} more {count === 1 ? "item" : "items"}</div>;
}

function extractHtmlResource(content: unknown[], data: Record<string, unknown>, result: Record<string, unknown>): HtmlResource | null {
  const candidates = [
    ...content.flatMap((entry) => {
      const block = record(entry);
      const nested = record(block.resource);
      return [nested, block];
    }),
    record(result.resource),
    record(data.resource),
  ];
  for (const candidate of candidates) {
    const mimeType = firstString(candidate.mimeType, candidate.mime_type);
    const html = text(candidate.text ?? candidate.html);
    if (!isHtmlMime(mimeType) || !html) continue;
    return {
      html,
      mimeType,
      uri: firstString(candidate.uri, candidate.resourceUri, candidate.resource_uri),
    };
  }
  const directHtml = text(result.html ?? data.html);
  if (!directHtml) return null;
  return {
    html: directHtml,
    mimeType: "text/html",
    uri: firstString(result.resourceUri, result.resource_uri, data.resourceUri, data.resource_uri),
  };
}

function extractMetaResourceUri(meta: Record<string, unknown>): string {
  const ui = record(meta.ui);
  return firstString(
    ui.resourceUri,
    ui.resource_uri,
    meta["ui/resourceUri"],
    meta["ui/resource_uri"],
    meta["openai/outputTemplate"],
  );
}

function extractError(data: Record<string, unknown>, result: Record<string, unknown>, status: string): string {
  const error = record(data.error ?? result.error);
  const explicit = firstString(error.message, error.error, data.errorMessage, data.error_message);
  if (explicit) return explicit;
  if (result.isError === true || result.is_error === true) {
    return firstString(result.message, result.error) || "The MCP interface reported an error.";
  }
  return ["failed", "error", "cancelled", "canceled"].includes(status.toLocaleLowerCase())
    ? "The MCP call did not complete successfully."
    : "";
}

function suppressDuplicatedJsonText(content: unknown[], structuredContent: unknown): unknown[] {
  if (structuredContent === undefined || content.length !== 1) return content;
  const block = record(content[0]);
  if (text(block.type).toLocaleLowerCase() !== "text" || Object.keys(record(block.annotations)).length) return content;
  const raw = text(block.text);
  if (!raw || !["{", "["].includes(raw.trim()[0] ?? "")) return content;
  try {
    return safeJson(JSON.parse(raw)) === safeJson(structuredContent) ? [] : content;
  } catch {
    return content;
  }
}

function resourceDescriptor(value: Record<string, unknown>): ResourceDescriptor {
  return {
    annotations: record(value.annotations),
    blob: text(value.blob),
    description: text(value.description),
    mimeType: firstString(value.mimeType, value.mime_type),
    name: text(value.name),
    size: number(value.size),
    text: text(value.text),
    title: text(value.title),
    uri: text(value.uri),
  };
}

function annotationText(value: Record<string, unknown>): string {
  const audience = array(value.audience).map(text).filter(Boolean).join(", ");
  return [
    audience ? `audience=${audience}` : "",
    value.priority == null ? "" : `priority=${String(value.priority)}`,
    firstString(value.lastModified, value.last_modified) ? `lastModified=${firstString(value.lastModified, value.last_modified)}` : "",
  ].filter(Boolean).join("; ");
}

function sandboxDocument(html: string): string {
  const parser = new DOMParser();
  const documentValue = parser.parseFromString(html, "text/html");
  documentValue.querySelectorAll("base, script, iframe, object, embed, link, meta[http-equiv='refresh' i]").forEach((node) => node.remove());
  documentValue.querySelectorAll("*").forEach((node) => {
    for (const attribute of [...node.attributes]) {
      const name = attribute.name.toLocaleLowerCase();
      if (name.startsWith("on")) {
        node.removeAttribute(attribute.name);
        continue;
      }
      if (!["action", "formaction", "href", "ping", "poster", "src", "srcset", "xlink:href"].includes(name)) continue;
      const value = attribute.value.trim().toLocaleLowerCase();
      const fragmentReference = (name === "href" || name === "xlink:href") && value.startsWith("#");
      const embeddedMedia = (name === "src" || name === "poster") && (value.startsWith("data:") || value.startsWith("blob:"));
      if (!fragmentReference && !embeddedMedia) node.removeAttribute(attribute.name);
    }
  });
  const csp = documentValue.createElement("meta");
  csp.httpEquiv = "Content-Security-Policy";
  csp.content = "default-src 'none'; base-uri 'none'; connect-src 'none'; form-action 'none'; frame-src 'none'; child-src 'none'; object-src 'none'; img-src data: blob:; media-src data: blob:; font-src data:; style-src 'unsafe-inline'; script-src 'none'";
  documentValue.head.prepend(csp);
  const viewport = documentValue.createElement("meta");
  viewport.name = "viewport";
  viewport.content = "width=device-width, initial-scale=1";
  documentValue.head.append(viewport);
  const style = documentValue.createElement("style");
  style.textContent = "html{color-scheme:light dark;background:transparent}body{margin:0;min-height:100%;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;color:CanvasText;background:transparent}*{box-sizing:border-box}";
  documentValue.head.append(style);
  return `<!doctype html>${documentValue.documentElement.outerHTML}`;
}

function extractStructuredActions(value: unknown): StructuredAction[] {
  const root = record(value);
  const ui = record(root.ui);
  const candidates = array(root.actions ?? ui.actions);
  const actions: StructuredAction[] = [];
  const seen = new Set<string>();
  for (const candidate of candidates) {
    const entry = record(candidate);
    const action = firstString(entry.action, entry.id, entry.name);
    const label = firstString(entry.label, entry.title);
    const explicit = text(entry.type).toLocaleLowerCase() === "action" || Boolean(text(entry.action) && label);
    if (!explicit || !validActionName(action) || seen.has(action)) continue;
    const payload = firstDefined(entry.payload, entry.arguments, entry.input, entry.value) ?? null;
    if (!normalizeActionPayload(payload).ok) continue;
    seen.add(action);
    actions.push({
      action,
      disabled: entry.disabled === true,
      label: label || humanize(action),
      payload,
      tone: entry.danger === true || text(entry.tone).toLocaleLowerCase() === "danger" ? "danger" : "neutral",
    });
    if (actions.length >= 8) break;
  }
  return actions;
}

function normalizeActionPayload(value: unknown): ActionPayload {
  try {
    const serialized = JSON.stringify(value ?? null, (_key, entry) => typeof entry === "bigint" ? entry.toString() : entry);
    if (!serialized || serialized.length > MAX_ACTION_PAYLOAD_LENGTH) return { ok: false, value: null };
    return { ok: true, value: JSON.parse(serialized) };
  } catch {
    return { ok: false, value: null };
  }
}

function isTabular(value: unknown[]): value is Array<Record<string, unknown>> {
  if (value.length < 2 || value.length > 200) return false;
  const records = value.map(record);
  if (records.some((entry) => !Object.keys(entry).length)) return false;
  const keys = [...new Set(records.flatMap((entry) => Object.keys(entry)))];
  if (!keys.length || keys.length > 8) return false;
  return records.every((entry) => keys.every((key) => scalar(entry[key])));
}

function scalar(value: unknown): boolean {
  return value == null || ["string", "number", "boolean"].includes(typeof value);
}

function isHtmlMime(value: string): boolean {
  const normalized = value.toLocaleLowerCase();
  return normalized === "text/html" || normalized.startsWith("text/html;") || normalized.includes("profile=mcp-app");
}

function isActiveStatus(value: string): boolean {
  return ["started", "starting", "running", "inprogress", "in_progress", "pending", "streaming"].includes(value.toLocaleLowerCase());
}

function validActionName(value: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value);
}

function endpointInterface(value: unknown): string {
  const endpoint = record(value);
  return firstString(endpoint.Interface, endpoint.interface);
}



function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function array(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function number(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function firstString(...values: unknown[]): string {
  return values.map(text).find(Boolean) ?? "";
}

function firstDefined(...values: unknown[]): unknown {
  return values.find((value) => value !== undefined);
}

function safeJson(value: unknown): string {
  try {
    return JSON.stringify(value, (_key, entry) => typeof entry === "bigint" ? entry.toString() : entry, 2) ?? "null";
  } catch {
    return "Unable to display this value.";
  }
}

function humanize(value: string): string {
  return value
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[._-]+/g, " ")
    .trim()
    .replace(/^./, (character) => character.toUpperCase());
}

function formatDuration(value: number): string {
  if (value < 1_000) return `${Math.round(value)} ms`;
  if (value < 60_000) return `${(value / 1_000).toFixed(value < 10_000 ? 1 : 0)} s`;
  return `${Math.floor(value / 60_000)}m ${Math.round((value % 60_000) / 1_000)}s`;
}

async function copyLocalText(value: string): Promise<void> {
  try {
    await copyText(value);
  } catch {
    return;
  }
}
