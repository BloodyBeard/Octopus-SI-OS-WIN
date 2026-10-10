import { useEffect, useMemo, useState } from "react";
import type { RunProjection, WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import { copyText } from "../../runtimeFormat";
import { appToast } from "../../toasts";
import { OcButton, OcCard, OcGroup, OcIconButton, OcSettingsRow } from "../../design-system/components";
import type { GlyphName } from "../IconGlyphs";
import { Glyph } from "../IconGlyphs";
import {
  OctopusLogBlock,
  OctopusNoteLine,
  OctopusPillRow,
  OctopusRow,
  OctopusSection,
  OctopusSurfaceShell,
} from "../OctopusSurface";
import { ChangesWorkspace, surfaceHasProjectedChanges } from "./ChangesWorkspace";
import { workerEnvironmentPanelKind } from "./surfaceRegistry";
import type { ProjectedAssistantCodeComment } from "../chat/assistantDirectives";
import {
  LiveSurfaceEmpty,
  ManualRoomControlWarning,
  dataOptionalNumber,
  dataRecordList,
  dataStringList,
  dataText,
  isViewerUrl,
  normalizeSurfaceStatus as normalizeStatus,
  renderSurfaceData,
} from "./RunEnvironmentSurfacePrimitives";

export function ChangesSurface({ active, assistantCodeComments = [], environment, onOpenFile, run, surface }: {
  active: boolean;
  assistantCodeComments?: ProjectedAssistantCodeComment[];
  environment: WorkerEnvironmentProjection;
  onOpenFile: (filePath: string, permanent?: boolean, surface?: WorkerEnvironmentSurfaceProjection) => void;
  run: RunProjection | null;
  surface?: WorkerEnvironmentSurfaceProjection;
}) {
  if (surface && (
    surfaceHasProjectedChanges(surface)
    || (surface.mode === "direct" && surface.protocol.trim().toLowerCase() === "git.worktree")
  )) {
    return (
      <ChangesWorkspace
        active={active}
        assistantCodeComments={assistantCodeComments}
        emptyIllustration={<ChangesEmptyIllustration />}
        fileSurface={compatibleReviewFileSurface(environment, surface)}
        onOpenFile={onOpenFile}
        projectionRevision={environment.revision}
        runId={run?.id || environment.runId}
        surface={surface}
        workerEnvironmentId={environment.id}
      />
    );
  }
  return (
    <div className="we-changes-shell">
      <LiveSurfaceEmpty
        detail="Changes in this Worker Environment will appear here when a worktree diff is projected."
        illustration={<ChangesEmptyIllustration />}
        title="No file changes yet"
      />
    </div>
  );
}

function compatibleReviewFileSurface(
  environment: WorkerEnvironmentProjection,
  reviewSurface: WorkerEnvironmentSurfaceProjection,
): WorkerEnvironmentSurfaceProjection | undefined {
  const reviewRoot = reviewSurface.root.trim();
  if (!reviewRoot) return undefined;
  return environment.surfaces.find((candidate) => (
    workerEnvironmentPanelKind(candidate) === "files"
    && candidate.mode === reviewSurface.mode
    && candidate.root.trim() === reviewRoot
  ));
}

function ChangesEmptyIllustration() {
  return (
    <svg aria-hidden="true" fill="none" viewBox="0 0 66 73" xmlns="http://www.w3.org/2000/svg">
      <path
        d="M20.4622 0.247806C21.3984 -0.00979114 22.5424 -0.0833059 24.3919 0.107181C26.2731 0.300998 28.6338 0.734691 31.9925 1.35718L50.5852 4.80249C53.6017 5.36157 54.6803 5.57925 55.6038 5.99488L55.929 6.15015C56.6787 6.52555 57.3664 7.00409 57.9681 7.57202C58.6934 8.25736 59.2703 9.14603 60.8177 11.6287L62.7884 14.7898C64.336 17.2728 64.8793 18.1822 65.1751 19.1355C65.455 20.0387 65.5807 20.9906 65.5491 21.9519C65.5479 21.9883 65.5432 22.0246 65.5413 22.0613C65.5596 22.3428 65.5672 22.6264 65.5579 22.9109V22.9119C65.5243 23.9209 65.245 24.9841 64.4183 27.9392L56.0804 57.7429C55.1602 61.0318 54.5093 63.3424 53.8548 65.1169V65.1179C53.2117 66.8608 52.6407 67.8608 51.9915 68.5945L51.9905 68.5955C50.6374 70.1236 48.849 71.2391 46.8811 71.781C45.9363 72.0411 44.7864 72.1129 42.9378 71.9226C41.0562 71.7288 38.6945 71.295 35.3362 70.6726L14.0296 66.7234C10.6714 66.101 8.31217 65.6599 6.51298 65.1716C4.74539 64.6918 3.74685 64.2213 3.03348 63.6541C1.54801 62.4722 0.529247 60.8364 0.122352 58.9822V58.9812C0.00960176 58.4665 -0.0292753 57.8806 0.0227425 57.1306C-0.0250512 56.373 0.0534353 55.4382 0.303016 54.1472C0.657029 52.3165 1.29974 50.004 2.22001 46.7146L11.302 14.2527C12.2224 10.9631 12.8721 8.65292 13.5266 6.87867C14.1702 5.13425 14.7404 4.13988 15.3841 3.41285C16.7292 1.89375 18.5059 0.78636 20.4622 0.247806ZM42.9808 70.9324C43.6743 71.0038 44.2688 71.0384 44.7903 71.0398C44.2691 71.0384 43.675 71.0038 42.9817 70.9324C42.7465 70.9081 42.5034 70.88 42.2522 70.8484L42.9808 70.9324ZM9.73075 64.908C10.9652 65.1573 12.3945 65.4229 14.0735 65.7341L35.3802 69.6824C37.4793 70.0714 39.1889 70.3869 40.635 70.616L39.7347 70.4675C38.4898 70.2571 37.0602 69.9936 35.3811 69.6824L14.0745 65.7341C12.3951 65.4229 10.9652 65.1573 9.73075 64.908ZM24.3411 0.604251C22.523 0.41699 21.4475 0.494741 20.595 0.729251C18.7322 1.24208 17.039 2.29737 15.7581 3.7439C15.1719 4.40597 14.6292 5.33725 13.9964 7.05249C13.3505 8.80345 12.7059 11.0904 11.7835 14.3875L2.70145 46.8494C1.77899 50.1466 1.14345 52.4359 0.794227 54.2419C0.452241 56.0108 0.446252 57.0412 0.622352 57.8445C1.00742 59.5997 1.972 61.1476 3.37821 62.2664C4.02189 62.7782 4.95004 63.227 6.68876 63.699C8.46395 64.1808 10.799 64.618 14.1653 65.2419L35.472 69.1912C38.8383 69.8151 41.176 70.2441 43.0325 70.4353C44.8509 70.6226 45.9261 70.545 46.7786 70.3103C48.6414 69.7974 50.3347 68.7422 51.6155 67.2957C52.2015 66.6336 52.7446 65.7028 53.3772 63.988C54.0232 62.237 54.6677 59.9493 55.5901 56.6521L63.928 26.8484C64.3266 25.4238 64.5906 24.4522 64.764 23.7244L53.3714 21.4041C52.1934 21.1641 51.6039 21.0432 51.2161 20.7195C50.9667 20.5111 50.7735 20.2461 50.6507 19.949C50.3911 19.6602 50.208 19.3084 50.1243 18.9255C50.06 18.6309 50.081 18.3236 50.1536 17.9578C50.2257 17.5945 50.3541 17.1493 50.5188 16.5759L53.5852 5.90015C52.8637 5.73959 51.8946 5.55438 50.4934 5.29468L31.9007 1.84839C28.5347 1.22455 26.1975 0.795542 24.3411 0.604251ZM48.5755 70.1746C48.2997 70.3039 48.0182 70.4211 47.7317 70.5261C48.0182 70.421 48.2997 70.3039 48.5755 70.1746ZM49.0227 69.9539L49.0218 69.9548L49.0227 69.9539ZM50.5677 68.9568C50.4229 69.0691 50.2746 69.1764 50.1243 69.281C50.2746 69.1764 50.4229 69.0691 50.5677 68.9568ZM51.8548 67.7722C51.771 67.8633 51.6848 67.9519 51.5979 68.0398C51.6848 67.9519 51.771 67.8633 51.8548 67.7722ZM0.428016 58.9783C0.944949 60.4218 1.85125 61.691 3.06669 62.658C3.78719 63.2307 4.79047 63.7019 6.55692 64.1814C6.78191 64.2425 7.01573 64.303 7.25907 64.363L6.5579 64.1814C5.23267 63.8217 4.33703 63.4667 3.66825 63.0701C3.55675 63.004 3.45169 62.9366 3.35184 62.8679C3.25212 62.7993 3.15765 62.7295 3.06766 62.658C2.0389 61.8396 1.23176 60.8043 0.694618 59.6306C0.645767 59.5239 0.59834 59.4164 0.553993 59.3074C0.509667 59.1984 0.467769 59.0884 0.428016 58.9773V58.9783ZM54.3792 60.9002C54.1696 61.606 53.9696 62.2451 53.7766 62.8298C54 62.1525 54.2305 61.4019 54.4778 60.5593C54.4441 60.6743 54.4123 60.7886 54.3792 60.9002ZM55.8245 57.6638C55.4675 58.9368 55.1526 60.0532 54.8587 61.0427C55.1526 60.0532 55.4675 58.9367 55.8245 57.6638ZM0.0764534 57.6433C0.0924277 57.7492 0.111172 57.8519 0.133094 57.9519C0.184295 58.1852 0.245507 58.4153 0.315711 58.6414L0.218055 58.2996C0.187644 58.1846 0.159687 58.0687 0.134071 57.9519C0.11215 57.8519 0.0928863 57.7491 0.0764534 57.6433ZM19.6155 45.2244C19.9624 43.9875 21.2673 43.1731 22.5306 43.407L36.8714 46.0652C38.1329 46.3007 38.8774 47.4939 38.5325 48.7302C38.1864 49.9672 36.8795 50.7801 35.6165 50.5476L21.2766 47.8904C20.0128 47.6561 19.2692 46.4622 19.6155 45.2244ZM34.096 22.2293C34.4424 20.9919 35.7486 20.1784 37.012 20.4119C38.2747 20.6469 39.0191 21.8407 38.6731 23.0779L37.3362 27.8572L42.219 28.7625C43.4806 28.9978 44.226 30.1911 43.8811 31.4275C43.5351 32.6645 42.2282 33.4774 40.9651 33.2449L36.0823 32.3406L34.7444 37.1228C34.3978 38.3595 33.0914 39.1732 31.8284 38.9402C30.5656 38.7055 29.8208 37.5113 30.1663 36.2742L31.5052 31.4919L26.6253 30.5877C25.3614 30.3533 24.6169 29.1595 24.9632 27.9216C25.3101 26.6846 26.6158 25.8702 27.8792 26.1043L32.7591 27.0085L34.096 22.2293ZM50.9993 16.7146C50.8323 17.296 50.7099 17.7177 50.6429 18.0554C50.5765 18.39 50.5694 18.6196 50.6126 18.8181C50.6955 19.1975 50.9025 19.5398 51.2005 19.7888C51.3566 19.919 51.5644 20.0181 51.8919 20.114C52.2222 20.2107 52.652 20.299 53.2444 20.4197L64.9554 22.8044C65.0101 22.4792 65.0411 22.2051 65.0501 21.9353C65.0799 21.0284 64.9606 20.1319 64.6975 19.283C64.4254 18.406 63.9256 17.5606 62.3636 15.0544L60.3938 11.8933C58.8317 9.38706 58.2918 8.56584 57.6243 7.93531C56.9781 7.32527 56.226 6.82357 55.3987 6.45093C55.0334 6.28652 54.6399 6.15562 54.0725 6.01441L50.9993 16.7146Z"
        fill="currentColor"
      />
    </svg>
  );
}

export { DesktopSurface } from "./DesktopSurface";

export function ConnectionSurface({ surface }: { surface?: WorkerEnvironmentSurfaceProjection }) {
  const [copied, setCopied] = useState(false);
  const address = surface ? dataText(surface.data, "address") || surface.viewerUrl : "";
  const facts = surface ? [
    ["Transport", dataText(surface.data, "transport") || surface.protocol],
    ["Workspace", dataText(surface.data, "workspace") || surface.root],
    ["Persistence", dataText(surface.data, "persistence")],
    ["Region", dataText(surface.data, "region")],
    ["Sync", dataText(surface.data, "sync")],
    ["Health", dataText(surface.data, "health")],
  ].filter((entry): entry is [string, string] => Boolean(entry[1])) : [];
  const events = surface ? dataRecordList(surface.data, "events") : [];
  const capabilities = surface ? dataStringList(surface.data, "capabilities") : [];

  if (!surface || (!address && facts.length === 0 && events.length === 0 && capabilities.length === 0)) {
    return (
      <LiveSurfaceEmpty
        detail="Connection state appears only when the formed Worker Environment projects its own view address and lifecycle evidence."
        glyph="interfaces"
        title="Connection descriptor unavailable"
      />
    );
  }

  async function copyAddress() {
    if (!address) return;
    try {
      if (!await copyText(address)) throw new Error("Clipboard access is unavailable.");
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch (error) {
      setCopied(false);
      appToast.warning(error instanceof Error ? error.message : String(error));
    }
  }

  const state = dataText(surface.data, "state") || surface.status;
  return (
    <OctopusSurfaceShell
      glyph="interfaces"
      status={state}
      statusTone={normalizeStatus(state) === "active" ? "active" : normalizeStatus(state) === "error" ? "error" : "muted"}
      subtitle={dataText(surface.data, "provider") || surface.protocol}
      title={dataText(surface.data, "name") || surface.label}
    >
      {address ? (
        <OctopusSection title="View address">
          <div className="flex min-h-9 items-center justify-between gap-2 px-3 py-1.5 text-sm">
            <code className="min-w-0 truncate font-mono text-xs text-token-foreground" title={address}>{address}</code>
            <span className="flex shrink-0 items-center">
              <OcIconButton
                glyph={copied ? "check" : "copy"}
                label={copied ? "Copied" : "Copy view address"}
                onClick={() => void copyAddress()}
                size="sm"
                variant="ghost"
              />
              {surface.mode === "direct" && isViewerUrl(surface.viewerUrl) ? (
                <OcIconButton
                  glyph="openExternal"
                  label="Open view address"
                  onClick={() => window.open(surface.viewerUrl, "_blank", "noopener,noreferrer")}
                  size="sm"
                  variant="ghost"
                />
              ) : null}
            </span>
          </div>
        </OctopusSection>
      ) : null}
      {facts.length ? (
        <OctopusSection title="Environment">
          {facts.map(([label, value]) => <OctopusRow key={label} label={label} value={value} />)}
        </OctopusSection>
      ) : null}
      <OctopusPillRow values={capabilities} />
      <OctopusSection count={events.length} title="Lifecycle">
        {events.length ? events.map((event, index) => (
          <div className="flex min-h-9 items-center gap-2.5 border-b border-token-border px-3 py-1.5 text-sm last:border-b-0" key={`${dataText(event, "time")}:${dataText(event, "label")}:${index}`}>
            <span className={`size-1.5 shrink-0 rounded-full ${normalizeStatus(dataText(event, "state")) === "active" ? "bg-token-charts-green" : normalizeStatus(dataText(event, "state")) === "error" ? "bg-token-charts-red" : "bg-token-description-foreground"}`} />
            <span className="flex min-w-0 flex-1 flex-col">
              <span className="truncate text-token-foreground">{dataText(event, "label") || "Environment event"}</span>
              {dataText(event, "detail") ? <span className="truncate text-xs text-token-text-secondary">{dataText(event, "detail")}</span> : null}
            </span>
            <span className="shrink-0 text-xs text-token-input-placeholder-foreground">{dataText(event, "time")}</span>
          </div>
        )) : <div className="px-3 py-2 text-sm text-token-description-foreground">No lifecycle events were projected.</div>}
      </OctopusSection>
    </OctopusSurfaceShell>
  );
}

export function ContainerSurface({ surface }: { surface?: WorkerEnvironmentSurfaceProjection }) {
  if (!surface || (surface.mode !== "fixture" && surface.mode !== "evidence")) {
    return <LiveSurfaceEmpty detail="Container state appears only when the Worker Environment projects an admitted runtime snapshot." glyph="ee" title="Container runtime unavailable" />;
  }
  const processes = dataRecordList(surface.data, "processes");
  const ports = dataRecordList(surface.data, "ports");
  const mounts = dataRecordList(surface.data, "mounts");
  return (
    <OctopusSurfaceShell
      glyph="ee"
      status={dataText(surface.data, "state") || surface.status}
      statusTone={surface.data.running === true ? "active" : "muted"}
      subtitle={dataText(surface.data, "image") || "No image evidence"}
      title={dataText(surface.data, "name") || surface.label}
    >
      <OctopusSection title="Runtime">
        <OctopusRow label="Health" value={dataText(surface.data, "health") || (surface.data.running === true ? "Running" : "—")} />
        <OctopusRow label="Exit code" value={String(dataOptionalNumber(surface.data, "exit_code") ?? "—")} />
        <OctopusRow label="Uptime" value={dataText(surface.data, "uptime") || "—"} />
        <OctopusRow label="Network" value={dataText(surface.data, "network") || "—"} />
      </OctopusSection>
      <OctopusSection count={processes.length} title="Processes">
        {processes.length ? processes.map((process, index) => (
          <div className="flex min-h-9 items-center gap-3 border-b border-token-border px-3 py-1.5 text-sm last:border-b-0" key={`${dataText(process, "pid")}:${dataText(process, "command")}:${index}`}>
            <span className="w-12 shrink-0 font-mono text-xs text-token-text-secondary">{dataText(process, "pid") || "—"}</span>
            <span className="min-w-0 flex-1 truncate font-mono text-xs text-token-foreground" title={dataText(process, "command")}>{dataText(process, "command") || "Unknown process"}</span>
            <span className="shrink-0 text-xs text-token-text-secondary">{dataText(process, "cpu") || "—"}</span>
            <span className="shrink-0 text-xs text-token-text-secondary">{dataText(process, "memory") || "—"}</span>
          </div>
        )) : <div className="px-3 py-2 text-sm text-token-description-foreground">No process snapshot was projected.</div>}
      </OctopusSection>
      <ContainerEvidenceList items={ports} keyField="port" title="Ports" valueField="service" />
      <ContainerEvidenceList items={mounts} keyField="target" title="Mounts" valueField="source" />
    </OctopusSurfaceShell>
  );
}

function ContainerEvidenceList({ items, keyField, title, valueField }: {
  items: Array<Record<string, unknown>>;
  keyField: string;
  title: string;
  valueField: string;
}) {
  return (
    <OctopusSection count={items.length} title={title}>
      {items.length ? items.map((item, index) => (
        <OctopusRow key={`${dataText(item, keyField)}:${index}`} label={dataText(item, keyField) || "—"} value={dataText(item, valueField) || "—"} />
      )) : <div className="px-3 py-2 text-sm text-token-description-foreground">None projected</div>}
    </OctopusSection>
  );
}

export function RuntimeSurface({ surface }: { surface?: WorkerEnvironmentSurfaceProjection }) {
  if (!surface || (surface.mode !== "fixture" && surface.mode !== "evidence")) {
    return <LiveSurfaceEmpty detail="Runtime state appears only when the formed room publishes admitted live-state evidence." glyph="pulse" title="Runtime evidence unavailable" />;
  }
  const activeCells = dataStringList(surface.data, "active_cells");
  const pendingCalls = dataRecordList(surface.data, "pending_tool_calls");
  const server = surface.data.server && typeof surface.data.server === "object" && !Array.isArray(surface.data.server)
    ? surface.data.server as Record<string, unknown>
    : null;
  const eventLog = dataText(surface.data, "event_log");
  const stderrLog = dataText(surface.data, "stderr_log");
  const error = dataText(surface.data, "error");
  const detail = dataText(surface.data, "detail");
  return (
    <OctopusSurfaceShell
      glyph="pulse"
      status={surface.status}
      statusTone={normalizeStatus(surface.status) === "active" ? "active" : normalizeStatus(surface.status) === "error" ? "error" : "muted"}
      subtitle={surface.protocol || undefined}
      title={surface.label}
    >
      {error ? <OctopusLogBlock label="Runtime error" tone="error">{error}</OctopusLogBlock> : null}
      {detail ? <OctopusNoteLine>{detail}</OctopusNoteLine> : null}
      {server ? (
        <OctopusSection title="Server">
          {Object.entries(server).map(([key, value]) => (
            <OctopusRow key={key} label={key.replace(/_/g, " ")} value={renderSurfaceData(value)} />
          ))}
        </OctopusSection>
      ) : null}
      {activeCells.length || pendingCalls.length ? (
        <OctopusSection count={`${activeCells.length} active`} title="Room activity">
          {activeCells.length ? (
            <div className="flex min-w-0 flex-wrap gap-1.5 border-b border-token-border px-3 py-2 last:border-b-0">
              {activeCells.map((cell) => <code className="rounded-md border border-token-border px-1.5 py-0.5 font-mono text-xs text-token-text-secondary" key={cell}>{cell}</code>)}
            </div>
          ) : null}
          {pendingCalls.map((call, index) => (
            <OctopusRow
              key={`${dataText(call, "id")}:${index}`}
              label={dataText(call, "tool") || dataText(call, "name") || `Pending call ${index + 1}`}
              value={dataText(call, "status") || "pending"}
            />
          ))}
        </OctopusSection>
      ) : null}
      {eventLog ? <OctopusLogBlock label="Runtime event log">{eventLog}</OctopusLogBlock> : null}
      {stderrLog ? <OctopusLogBlock label="Runtime standard error" tone="error">{stderrLog}</OctopusLogBlock> : null}
      {!server && !activeCells.length && !pendingCalls.length && !eventLog && !stderrLog && !error && !detail ? (
        <LiveSurfaceEmpty detail="The room is formed, but its current live-state carries no inspectable runtime payload." glyph="pulse" title="No runtime details" />
      ) : null}
    </OctopusSurfaceShell>
  );
}

export function CustomEnvironmentSurface({ surface }: { surface?: WorkerEnvironmentSurfaceProjection }) {
  if (!surface) return <LiveSurfaceEmpty detail="No surface descriptor was projected." glyph="plugins" title="Surface unavailable" />;
  const entries = Object.entries(surface.data);
  return (
    <OctopusSurfaceShell
      glyph="plugins"
      status={surface.status}
      statusTone={normalizeStatus(surface.status) === "active" ? "active" : normalizeStatus(surface.status) === "error" ? "error" : "muted"}
      subtitle={surface.protocol || surface.kind}
      title={surface.label}
    >
      {entries.length ? (
        <OctopusSection title="Projected data">
          {entries.map(([key, value]) => (
            <OctopusRow key={key} label={key.replace(/_/g, " ")} value={renderSurfaceData(value)} />
          ))}
        </OctopusSection>
      ) : (
        <LiveSurfaceEmpty detail="This custom surface descriptor carries no native client data." glyph="plugins" title={surface.label} />
      )}
    </OctopusSurfaceShell>
  );
}

export function EnvironmentOverview({ environment }: { environment: WorkerEnvironmentProjection }) {
  const facts = ([
    ["Declared kind", environment.kind || "Not projected"],
    ["Room lifecycle", environment.lifecycle || "Unknown"],
    ["Current process state", environment.status || "Unknown"],
    ["Session", environment.session || "No session attached"],
    ["Door", environment.door],
    ["Substrate", environment.substrateFamily],
    ["Transport", environment.transport],
  ] as const).flatMap(([label, value]) => value ? [{ label, value: String(value) }] : []);
  const roots = Array.from(new Map([
    ...(environment.root ? [[environment.root, environment.root] as const] : []),
    ...environment.surfaces.flatMap((surface) => surface.root ? [[
      surface.root,
      `${surface.root} · ${surface.readOnly ? "Read only" : "Read and write"}`,
    ] as const] : []),
  ]).values());
  return (
    <div className="we-overview">
      <div className="we-overview-content">
        <OcGroup
          description={environment.detail}
          title={environment.label || "Worker Environment"}
        >
          <OcCard className="we-overview-row-group">
            {facts.length ? facts.map(({ label, value }) => (
              <OcSettingsRow
                control={<span className="we-overview-value" title={value}>{value}</span>}
                key={label}
                label={label}
              />
            )) : <OcSettingsRow label="No environment details projected" />}
          </OcCard>
        </OcGroup>
        <OverviewList items={roots} title="Filesystem access" />
        <OverviewList items={environment.capabilities} title="Capabilities" />
        <OverviewList items={environment.tools} title="Tools through the door" />
        <OverviewList items={environment.surfaces.map((surface) => surface.label)} title="Projected surfaces" />
      </div>
    </div>
  );
}

type WorktreePhase = "queued" | "creating" | "setting-up" | "worktree-ready" | "failed";
type WorktreeActivityStatus = "waiting" | "running" | "completed" | "failed" | "skipped";

type WorktreeActivity = {
  id: "worktree" | "setup";
  label: string;
  status: WorktreeActivityStatus;
  output: string;
};

export function WorktreeSurface({ surface }: { surface?: WorkerEnvironmentSurfaceProjection }) {
  const projection = useMemo(() => parseWorktreeSurface(surface), [surface]);
  const fixture = surface?.mode === "fixture";
  const [fixturePhase, setFixturePhase] = useState<WorktreePhase | null>(projection.phase);

  useEffect(() => {
    setFixturePhase(projection.phase);
  }, [projection.phase, surface?.id]);

  useEffect(() => {
    if (!fixture || fixturePhase == null || fixturePhase === "failed" || fixturePhase === "worktree-ready") return;
    const next: WorktreePhase = fixturePhase === "queued"
      ? "creating"
      : fixturePhase === "creating"
        ? "setting-up"
        : "worktree-ready";
    const delay = fixturePhase === "queued" ? 360 : fixturePhase === "creating" ? 760 : 980;
    const timer = window.setTimeout(() => setFixturePhase(next), delay);
    return () => window.clearTimeout(timer);
  }, [fixture, fixturePhase]);

  if (!surface) {
    return <LiveSurfaceEmpty detail="No worktree status surface was projected." glyph="worktree" title="Worktree unavailable" />;
  }
  if (projection.phase == null || fixturePhase == null) {
    return (
      <LiveSurfaceEmpty
        detail="The projected worktree surface does not contain a supported phase. Expected queued, creating, setting-up, worktree-ready, or failed."
        glyph="worktree"
        title="Worktree state unavailable"
      />
    );
  }

  const phase = fixture ? fixturePhase : projection.phase;
  const activities = worktreeActivities(phase, projection);
  const phaseLabel = worktreePhaseLabel(phase);
  const failed = phase === "failed";

  return (
    <div className="we-worktree-shell">
      <div className="we-worktree-content">
        <header className="we-worktree-heading">
          <span className={`we-worktree-heading-icon is-${failed ? "failed" : phase === "worktree-ready" ? "ready" : "running"}`}>
            <Glyph name={failed ? "close" : phase === "worktree-ready" ? "check" : "worktree"} size={18} />
          </span>
          <div>
            <h2>{projection.label || surface.label}</h2>
            <p>{projection.description || "Persistent project worktree"}</p>
          </div>
          <span className={`we-worktree-phase is-${failed ? "failed" : phase === "worktree-ready" ? "ready" : "running"}`}>
            <i />{phaseLabel}
          </span>
        </header>

        <div className="we-worktree-activities" aria-label="Worktree setup activity">
          {activities.map((activity, index) => (
            <WorktreeActivityRow activity={activity} key={activity.id} last={index === activities.length - 1} />
          ))}
        </div>

        <WorktreeContext projection={projection} />

        {failed ? (
          <div className="we-worktree-failure" role="alert">
            <strong>Worktree setup failed</strong>
            <p>{projection.errorMessage || "The projected state did not include a failure message."}</p>
          </div>
        ) : null}

        <footer className="we-worktree-actions">
          <div>
            {projection.workspaceRoot ? <code title={projection.workspaceRoot}>{projection.workspaceRoot}</code> : null}
            {projection.branch ? <span>{projection.branch}</span> : null}
          </div>
          {fixture ? (
            <div>
              {failed ? (
                <OcButton leadingIcon="retry" onClick={() => setFixturePhase("queued")} variant="primary">Retry</OcButton>
              ) : (
                <OcButton disabled={phase !== "worktree-ready"} leadingIcon="refresh" onClick={() => setFixturePhase("queued")} variant="secondary">Replay setup</OcButton>
              )}
              <OcButton disabled={phase !== "worktree-ready"} onClick={() => setFixturePhase("failed")} tone="danger" variant="ghost">Simulate failure</OcButton>
            </div>
          ) : failed ? (
            <small>Repair actions require an explicit projected action target.</small>
          ) : null}
        </footer>
      </div>
    </div>
  );
}

function WorktreeContext({ projection }: { projection: ReturnType<typeof parseWorktreeSurface> }) {
  const repositoryValues = [projection.repository, projection.remoteUrl].filter(Boolean);
  const revisionValues = [
    projection.branch,
    projection.baseBranch && projection.baseBranch !== projection.branch ? `from ${projection.baseBranch}` : "",
    projection.headSha ? projection.headSha.slice(0, 12) : "",
  ].filter(Boolean);
  const changeCounts = [
    projection.stagedCount == null ? "" : `${projection.stagedCount} staged`,
    projection.unstagedCount == null ? "" : `${projection.unstagedCount} changed`,
    projection.untrackedCount == null ? "" : `${projection.untrackedCount} untracked`,
  ].filter(Boolean);
  const rows = [
    { glyph: "worktree" as GlyphName, label: "Repository", values: repositoryValues },
    { glyph: "changes" as GlyphName, label: "Revision", values: revisionValues },
    { glyph: "folder" as GlyphName, label: "Workspace", values: [projection.workspaceRoot].filter(Boolean) },
    { glyph: "pulse" as GlyphName, label: "Working tree", values: changeCounts },
  ].filter((row) => row.values.length);
  if (!rows.length && !projection.linkedRuns.length) return null;
  return (
    <section className="we-worktree-context" aria-label="Repository context">
      {rows.map((row) => (
        <div key={row.label}>
          <span><Glyph name={row.glyph} size={13} /></span>
          <strong>{row.label}</strong>
          <p title={row.values.join(" · ")}>{row.values.join(" · ")}</p>
        </div>
      ))}
      {projection.linkedRuns.length ? (
        <div className="we-worktree-linked-runs">
          <span><Glyph name="chat" size={13} /></span>
          <strong>Linked runs</strong>
          <p>{projection.linkedRuns.join(" · ")}</p>
        </div>
      ) : null}
    </section>
  );
}

function WorktreeActivityRow({ activity, last }: { activity: WorktreeActivity; last: boolean }) {
  return (
    <article className={`we-worktree-activity is-${activity.status}`}>
      <div className="we-worktree-activity-rail">
        <span>
          {activity.status === "completed" ? <Glyph name="check" size={12} />
            : activity.status === "failed" ? <Glyph name="close" size={12} />
              : activity.status === "running" ? <i className="we-worktree-spinner" />
                : <i />}
        </span>
        {!last ? <b /> : null}
      </div>
      <div className="we-worktree-activity-copy">
        <div>
          <strong>{activity.label}</strong>
          <small>{worktreeActivityStatusLabel(activity.status)}</small>
        </div>
        {activity.output ? (
          <details>
            <summary>View output</summary>
            <pre>{activity.output}</pre>
          </details>
        ) : null}
      </div>
    </article>
  );
}

function parseWorktreeSurface(surface?: WorkerEnvironmentSurfaceProjection) {
  const data = surface?.data ?? {};
  const phaseValue = dataText(data, "phase").toLowerCase();
  const phase: WorktreePhase | null = ["queued", "creating", "setting-up", "worktree-ready", "failed"].includes(phaseValue)
    ? phaseValue as WorktreePhase
    : null;
  return {
    phase,
    label: dataText(data, "label") || dataText(data, "name"),
    description: dataText(data, "description"),
    branch: dataText(data, "branch"),
    baseBranch: dataText(data, "base_branch") || dataText(data, "baseBranch"),
    headSha: dataText(data, "head_sha") || dataText(data, "headSha") || dataText(data, "sha"),
    repository: dataText(data, "repository") || dataText(data, "repo") || dataText(data, "repository_name"),
    remoteUrl: dataText(data, "remote_url") || dataText(data, "remoteUrl") || dataText(data, "origin_url"),
    stagedCount: dataOptionalNumber(data, "staged_count") ?? dataOptionalNumber(data, "stagedCount"),
    unstagedCount: dataOptionalNumber(data, "unstaged_count") ?? dataOptionalNumber(data, "unstagedCount"),
    untrackedCount: dataOptionalNumber(data, "untracked_count") ?? dataOptionalNumber(data, "untrackedCount"),
    linkedRuns: dataStringList(data, "linked_runs").length ? dataStringList(data, "linked_runs") : dataStringList(data, "linkedRuns"),
    gitRoot: dataText(data, "worktree_git_root") || dataText(data, "git_root"),
    workspaceRoot: dataText(data, "worktree_workspace_root") || dataText(data, "workspace_root"),
    configPath: dataText(data, "local_environment_config_path") || dataText(data, "config_path"),
    errorMessage: dataText(data, "error_message") || dataText(data, "error"),
    worktreeOutput: dataText(data, "worktree_output_text") || dataText(data, "worktree_output"),
    setupOutput: dataText(data, "setup_output_text") || dataText(data, "setup_output"),
  };
}

function worktreeActivities(phase: WorktreePhase, projection: ReturnType<typeof parseWorktreeSurface>): WorktreeActivity[] {
  const rootsReady = Boolean(projection.gitRoot && projection.workspaceRoot);
  const worktreeStatus: WorktreeActivityStatus = phase === "queued" || phase === "creating"
    ? "running"
    : phase === "failed" && !rootsReady
      ? "failed"
      : "completed";
  const setupStatus: WorktreeActivityStatus = phase === "queued" || phase === "creating"
    ? "waiting"
    : phase === "setting-up"
      ? "running"
      : phase === "failed"
        ? rootsReady ? "failed" : "skipped"
        : projection.configPath && projection.errorMessage
          ? "skipped"
          : "completed";
  return [
    { id: "worktree", label: "Create worktree", status: worktreeStatus, output: projection.worktreeOutput },
    { id: "setup", label: "Set up environment", status: setupStatus, output: projection.setupOutput },
  ];
}

function worktreePhaseLabel(phase: WorktreePhase): string {
  if (phase === "queued") return "Queued";
  if (phase === "creating") return "Creating worktree";
  if (phase === "setting-up") return "Running setup";
  if (phase === "worktree-ready") return "Ready";
  return "Failed";
}

function worktreeActivityStatusLabel(status: WorktreeActivityStatus): string {
  if (status === "waiting") return "Waiting";
  if (status === "running") return "In progress";
  if (status === "completed") return "Completed";
  if (status === "skipped") return "Skipped";
  return "Failed";
}

function OverviewList({ items, title }: { items: string[]; title: string }) {
  return (
    <OcGroup title={title}>
      <OcCard className="we-overview-row-group">
        {items.length
          ? items.map((item) => <OcSettingsRow key={item} label={item} />)
          : <OcSettingsRow label="None projected" />}
      </OcCard>
    </OcGroup>
  );
}
