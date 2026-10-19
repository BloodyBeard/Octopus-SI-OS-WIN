import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type MutableRefObject,
  type Ref,
  type ReactNode,
} from "react";
import type { OctopusClientAccessProfile } from "../../electron";
import type {
  ClientExtensionCategory,
  ClientExtensionCredentialCheck,
  ClientExtensionRecord,
  ClientExtensionRegistryState,
  ClientExtensionSourceConfiguration,
} from "../../extensions/types";
import {
  OcAnimatedActionSlot,
  OcButton,
  OcDialogFooter,
  OcEmptyState,
  OcIconButton,
  OcSearchField,
  OcSelect,
  OcStatus,
  OcTextField,
  cx,
} from "../../design-system/components";
import { appToast, useAppErrorToast } from "../../toasts";
import { useClientPreferences, useSetClientPreferences } from "../../clientPreferences";
import {
  CatalogInstalledRail,
  CatalogLoadingState,
  CatalogRouteTransition,
  CatalogSection,
} from "../catalog";
import { Glyph } from "../IconGlyphs";
import { OctopusLoadingState } from "../navigation/OctopusLoadingState";
import { SetupDialogShell } from "../SetupDialogShell";
import { SettingsHeading } from "./SettingsHeading";

const CATEGORY_COPY: Record<ClientExtensionCategory, { title: string; description: string }> = {
  widget: {
    title: "Widgets",
    description: "Useful at-a-glance information on Home.",
  },
  tab: {
    title: "Tabs & panels",
    description: "Dedicated spaces for tools and information.",
  },
  repair: {
    title: "Repairs",
    description: "Guided fixes offered when Octopus recognizes a problem.",
  },
  input: {
    title: "Input",
    description: "New ways to control the client with your hands or voice.",
  },
};

export function ExtensionsSection({
  accessProfile,
  registry,
}: {
  accessProfile: OctopusClientAccessProfile;
  registry: ClientExtensionRegistryState;
}) {
  const clientPreferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  const searchRef = useRef<HTMLInputElement>(null);
  const cardRefs = useRef(new Map<string, HTMLDivElement>());
  const [browseQuery, setBrowseQuery] = useState("");
  const [manageQuery, setManageQuery] = useState("");
  const [catalogView, setCatalogView] = useState<"browse" | "manage">("browse");
  const [detailReturnView, setDetailReturnView] = useState<"browse" | "manage">("browse");
  const [routeDirection, setRouteDirection] = useState<"back" | "forward">("forward");
  const [selectedId, setSelectedId] = useState("");
  const [highlightedId, setHighlightedId] = useState("");
  const [setupId, setSetupId] = useState("");
  const [installAfterSetup, setInstallAfterSetup] = useState(false);
  const [installSources, setInstallSources] = useState<{
    extensionId: string;
    value: Record<string, ClientExtensionSourceConfiguration>;
    credentials: Record<string, string>;
  } | null>(null);
  const [installId, setInstallId] = useState("");
  const [uninstallId, setUninstallId] = useState("");
  const [busyId, setBusyId] = useState("");
  const [actionError, setActionError] = useState("");
  useAppErrorToast(actionError);

  const visible = useMemo(
    () => rankExtensions(registry.extensions, browseQuery),
    [browseQuery, registry.extensions],
  );
  const installed = useMemo(
    () => registry.extensions.filter((extension) => extension.installed),
    [registry.extensions],
  );
  const visibleInstalled = useMemo(
    () => rankExtensions(installed, manageQuery),
    [installed, manageQuery],
  );
  const selected = registry.extensions.find((extension) => extension.manifest.id === selectedId) ?? null;
  const setup = registry.extensions.find((extension) => extension.manifest.id === setupId) ?? null;
  const install = registry.extensions.find((extension) => extension.manifest.id === installId) ?? null;
  const uninstall = registry.extensions.find((extension) => extension.manifest.id === uninstallId) ?? null;
  const detected = registry.extensions.find((extension) => (
    extension.admission === "pending" && extension.lifecycle.receiptState !== "invalid"
  )) ?? null;
  const keyboardIds = (catalogView === "manage" ? visibleInstalled : visible)
    .map((extension) => extension.manifest.id);

  useEffect(() => {
    if (selectedId && !selected) {
      setSelectedId("");
      setRouteDirection("back");
    }
  }, [selected, selectedId]);

  useEffect(() => {
    if (!registry.mutable || !detected || setupId || installId || uninstallId || busyId) return;
    const needsSetup = (detected.manifest.sources ?? []).some((source) => source.required)
      && !detected.configured;
    if (needsSetup) {
      setInstallAfterSetup(true);
      setSetupId(detected.manifest.id);
      return;
    }
    setInstallId(detected.manifest.id);
  }, [busyId, detected, installId, registry.mutable, setupId, uninstallId]);

  function onSearchKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    const renderedKeyboardIds = keyboardIds.filter((id) => cardRefs.current.get(id)?.isConnected);
    if (!renderedKeyboardIds.length || !["ArrowDown", "ArrowUp", "Enter"].includes(event.key)) return;
    event.preventDefault();
    const current = Math.max(0, renderedKeyboardIds.indexOf(highlightedId));
    if (event.key === "Enter") {
      openDetail(highlightedId || renderedKeyboardIds[current], catalogView);
      return;
    }
    const nextIndex = event.key === "ArrowDown"
      ? (current + (highlightedId ? 1 : 0)) % renderedKeyboardIds.length
      : (current - 1 + renderedKeyboardIds.length) % renderedKeyboardIds.length;
    const next = renderedKeyboardIds[nextIndex];
    setHighlightedId(next);
    cardRefs.current.get(next)?.scrollIntoView({ block: "nearest" });
  }

  async function perform(id: string, action: () => Promise<void>) {
    setBusyId(id);
    try {
      await action();
      setActionError("");
    } catch (error) {
      setActionError(error instanceof Error ? error.message : String(error));
    } finally {
      setBusyId("");
    }
  }

  function requestInstall(extension: ClientExtensionRecord) {
    if (!registry.mutable || extension.lifecycle.receiptState === "invalid") return;
    setInstallSources(null);
    const needsSetup = (extension.manifest.sources ?? []).some((source) => source.required)
      && !extension.configured;
    if (needsSetup) {
      setInstallAfterSetup(true);
      setSetupId(extension.manifest.id);
      return;
    }
    if (extension.admission === "builtin") {
      void perform(extension.manifest.id, async () => {
        await registry.install(extension.manifest.id);
        revealInstalledTab(extension, clientPreferences.hiddenSurfaceIds, setClientPreferences);
      });
      return;
    }
    setInstallId(extension.manifest.id);
  }

  function openDetail(id: string, returnView: "browse" | "manage" = catalogView) {
    setDetailReturnView(returnView);
    setRouteDirection("forward");
    setSelectedId(id);
  }

  function closeDetail() {
    setCatalogView(detailReturnView);
    setRouteDirection("back");
    setSelectedId("");
  }

  function openManage() {
    setHighlightedId("");
    setRouteDirection("forward");
    setCatalogView("manage");
  }

  function closeManage() {
    setHighlightedId("");
    setRouteDirection("back");
    setCatalogView("browse");
  }

  return (
    <section className="settings-section settings-extensions-section flex flex-col">
      <CatalogRouteTransition
        direction={routeDirection}
        viewKey={selected ? `extension-detail:${selected.manifest.id}` : `extension-${catalogView}`}
      >
        {selected ? (
          <ExtensionDetail
            busy={busyId === selected.manifest.id}
            extension={selected}
            mutable={registry.mutable}
            onBack={closeDetail}
            onInstall={() => requestInstall(selected)}
            onSetup={() => {
              setInstallAfterSetup(false);
              setSetupId(selected.manifest.id);
            }}
            onUninstall={() => setUninstallId(selected.manifest.id)}
          />
        ) : catalogView === "manage" ? (
          <ExtensionManageView
            busyId={busyId}
            cardRefs={cardRefs}
            extensions={visibleInstalled}
            highlightedId={highlightedId}
            mutable={registry.mutable}
            onBack={closeManage}
            onOpen={(id) => openDetail(id, "manage")}
            onOpenFolder={() => void perform("folder", registry.revealRoot)}
            onQueryChange={(value) => {
              setManageQuery(value);
              setHighlightedId("");
            }}
            onSearchKeyDown={onSearchKeyDown}
            query={manageQuery}
            registry={registry}
            searchRef={searchRef}
          />
        ) : (
          <ExtensionBrowseView
            busyId={busyId}
            cardRefs={cardRefs}
            extensions={visible}
            highlightedId={highlightedId}
            installed={installed}
            mutable={registry.mutable}
            onInstall={requestInstall}
            onManage={openManage}
            onOpen={(id) => openDetail(id, "browse")}
            onOpenFolder={() => void perform("folder", registry.revealRoot)}
            onQueryChange={(value) => {
              setBrowseQuery(value);
              setHighlightedId("");
            }}
            onSearchKeyDown={onSearchKeyDown}
            query={browseQuery}
            registry={registry}
            searchRef={searchRef}
          />
        )}
      </CatalogRouteTransition>

      <ExtensionSetupDialog
        accessProfile={accessProfile}
        busy={Boolean(setup && busyId === setup.manifest.id)}
        extension={setup}
        installAfterSave={installAfterSetup}
        onCancel={() => {
          if (setup?.admission === "pending") {
            void perform(setup.manifest.id, async () => {
              await registry.dismiss(setup.manifest.id);
              setSetupId("");
              setInstallAfterSetup(false);
              setInstallSources(null);
            });
            return;
          }
          setSetupId("");
          setInstallAfterSetup(false);
          setInstallSources(null);
        }}
        onCheckCredential={(sourceId) => setup
          ? registry.checkCredential(setup.manifest.id, sourceId)
          : Promise.reject(new Error("No extension is being set up."))}
        onRemoveCredential={(sourceId) => {
          if (!setup) return;
          void perform(setup.manifest.id, () => registry.removeCredential(setup.manifest.id, sourceId));
        }}
        onSave={(sources, credentials) => {
          if (!setup) return;
          void perform(setup.manifest.id, async () => {
            if (installAfterSetup) {
              setInstallSources({ extensionId: setup.manifest.id, value: sources, credentials });
              setInstallId(setup.manifest.id);
            } else {
              await registry.configure(setup.manifest.id, sources, false, undefined, credentials);
            }
            setSetupId("");
            setInstallAfterSetup(false);
          });
        }}
      />

      <ExtensionUninstallDialog
        busy={Boolean(uninstall && busyId === uninstall.manifest.id)}
        extension={uninstall}
        onCancel={() => setUninstallId("")}
        onConfirm={() => {
          if (!uninstall) return;
          const removed = uninstall;
          void perform(uninstall.manifest.id, async () => {
            await registry.uninstall(uninstall.manifest.id);
            setUninstallId("");
            if (selectedId === uninstall.manifest.id) {
              setSelectedId("");
              setRouteDirection("back");
            }
            appToast.info(`${removed.manifest.name} removed`, 8, {
              dedupeKey: `extension-uninstall:${removed.manifest.id}`,
              action: {
                label: "Undo",
                onSelect: () => {
                  void perform(removed.manifest.id, async () => {
                    await registry.install(removed.manifest.id);
                  });
                },
              },
            });
          });
        }}
      />
      <ExtensionInstallDialog
        busy={Boolean(install && busyId === install.manifest.id)}
        extension={install}
        onCancel={() => {
          if (install?.admission === "pending") {
            void perform(install.manifest.id, async () => {
              await registry.dismiss(install.manifest.id);
              setInstallId("");
              setInstallSources(null);
            });
            return;
          }
          setInstallId("");
        }}
        onConfirm={() => {
          if (!install) return;
          void perform(install.manifest.id, async () => {
            if (installSources?.extensionId === install.manifest.id) {
              await registry.configure(install.manifest.id, installSources.value, true, undefined, installSources.credentials);
            }
            else await registry.install(install.manifest.id);
            revealInstalledTab(install, clientPreferences.hiddenSurfaceIds, setClientPreferences);
            setInstallId("");
            setInstallSources(null);
          });
        }}
      />
    </section>
  );
}

function ExtensionBrowseView({
  busyId,
  cardRefs,
  extensions,
  highlightedId,
  installed,
  mutable,
  onInstall,
  onManage,
  onOpen,
  onOpenFolder,
  onQueryChange,
  onSearchKeyDown,
  query,
  registry,
  searchRef,
}: {
  busyId: string;
  cardRefs: MutableRefObject<Map<string, HTMLDivElement>>;
  extensions: ClientExtensionRecord[];
  highlightedId: string;
  installed: ClientExtensionRecord[];
  mutable: boolean;
  onInstall: (extension: ClientExtensionRecord) => void;
  onManage: () => void;
  onOpen: (id: string) => void;
  onOpenFolder: () => void;
  onQueryChange: (value: string) => void;
  onSearchKeyDown: (event: KeyboardEvent<HTMLInputElement>) => void;
  query: string;
  registry: ClientExtensionRegistryState;
  searchRef: Ref<HTMLInputElement>;
}) {
  return (
    <div>
      <SettingsHeading
        actions={(
          <OcButton
            disabled={!registry.mutable || !registry.root}
            onClick={onOpenFolder}
            size="sm"
            variant="ghost"
          >
            Open folder
          </OcButton>
        )}
        description="Add focused capabilities to Home and other parts of Octopus. Extensions stay local to this app."
        title="Extensions"
      />
      <ExtensionCatalogSearch
        id="extensions-settings-search"
        inputRef={searchRef}
        onChange={onQueryChange}
        onKeyDown={onSearchKeyDown}
        placeholder="Search extensions"
        value={query}
      />
      <div className="flex flex-col gap-8 pt-3">
        {!registry.mutable ? <ExtensionReadOnlyNotice /> : null}
        {registry.loading ? (
          <CatalogLoadingState label="Loading extensions…" />
        ) : registry.errors.length && registry.extensions.length === 0 ? (
          <OcEmptyState detail={registry.errors.join("\n")} glyph="warning" size="compact" title="Extensions could not be loaded" tone="danger" />
        ) : extensions.length === 0 ? (
          <OcEmptyState glyph="search" size="compact" title="No extensions found" />
        ) : (
          <>
            {installed.length ? (
              <CatalogInstalledRail
                getItemKey={extensionKey}
                getItemLabel={extensionName}
                items={installed}
                manageLabel="Manage installed extensions"
                onManage={onManage}
                onOpen={(extension) => onOpen(extension.manifest.id)}
                renderIcon={(extension, size) => (
                  <ExtensionVisual className="size-full" extension={extension} size={size === "tile" ? 18 : 8} />
                )}
              />
            ) : null}
            {(["widget", "tab", "input", "repair"] as const).map((category) => {
              const rows = extensions.filter((extension) => extension.manifest.category === category);
              if (!rows.length) return null;
              const copy = CATEGORY_COPY[category];
              return (
                <CatalogSection
                  batchSize={12}
                  description={copy.description}
                  getItemKey={extensionKey}
                  getItemLabel={extensionName}
                  items={rows}
                  key={category}
                  renderCard={(extension) => (
                    <ExtensionCard
                      busy={busyId === extension.manifest.id}
                      extension={extension}
                      highlighted={highlightedId === extension.manifest.id}
                      mutable={mutable}
                      onInstall={() => onInstall(extension)}
                      onOpen={() => onOpen(extension.manifest.id)}
                      setRef={(node) => setExtensionCardRef(cardRefs, extension.manifest.id, node)}
                    />
                  )}
                  renderItemPreview={(extension) => <Glyph name={extension.manifest.glyph} size={8} />}
                  resetKey={`${category}:${query}`}
                  title={copy.title}
                  visibleItemLimit={6}
                />
              );
            })}
          </>
        )}
        {!registry.loading && registry.errors.length && registry.extensions.length > 0 ? (
          <ExtensionRegistryErrors errors={registry.errors} />
        ) : null}
      </div>
    </div>
  );
}

function ExtensionManageView({
  busyId,
  cardRefs,
  extensions,
  highlightedId,
  mutable,
  onBack,
  onOpen,
  onOpenFolder,
  onQueryChange,
  onSearchKeyDown,
  query,
  registry,
  searchRef,
}: {
  busyId: string;
  cardRefs: MutableRefObject<Map<string, HTMLDivElement>>;
  extensions: ClientExtensionRecord[];
  highlightedId: string;
  mutable: boolean;
  onBack: () => void;
  onOpen: (id: string) => void;
  onOpenFolder: () => void;
  onQueryChange: (value: string) => void;
  onSearchKeyDown: (event: KeyboardEvent<HTMLInputElement>) => void;
  query: string;
  registry: ClientExtensionRegistryState;
  searchRef: Ref<HTMLInputElement>;
}) {
  const hasInstalledExtensions = registry.extensions.some((extension) => extension.installed);
  return (
    <div>
      <button className="mb-5 flex w-fit cursor-interaction items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-token-text-secondary hover:bg-token-list-hover-background hover:text-token-foreground" onClick={onBack} type="button">
        <Glyph name="arrowLeft" size={14} />
        <span>Extensions</span>
      </button>
      <SettingsHeading
        actions={(
          <OcButton
            disabled={!registry.mutable || !registry.root}
            onClick={onOpenFolder}
            size="sm"
            variant="ghost"
          >
            Open folder
          </OcButton>
        )}
        description="Set up or remove extensions already installed in this client."
        title="Manage extensions"
      />
      <ExtensionCatalogSearch
        id="extensions-manage-search"
        inputRef={searchRef}
        onChange={onQueryChange}
        onKeyDown={onSearchKeyDown}
        placeholder="Search installed extensions"
        value={query}
      />
      <div className="flex flex-col gap-8 pt-3">
        {!registry.mutable ? <ExtensionReadOnlyNotice /> : null}
        {registry.loading ? (
          <OctopusLoadingState>Loading installed extensions…</OctopusLoadingState>
        ) : registry.errors.length && registry.extensions.length === 0 ? (
          <OcEmptyState detail={registry.errors.join("\n")} glyph="warning" size="compact" title="Extensions could not be loaded" tone="danger" />
        ) : extensions.length === 0 ? (
          <OcEmptyState
            glyph={query ? "search" : "plugins"}
            size="compact"
            title={query && hasInstalledExtensions ? "No installed extensions found" : "No extensions installed"}
          />
        ) : (
          <>
            <div className="[&_.oc-catalog-grid]:!grid-cols-1">
              <CatalogSection
                batchSize={12}
                description="Open an extension to review its setup, sources, or removal options."
                getItemKey={extensionKey}
                getItemLabel={extensionName}
                items={extensions}
                renderCard={(extension) => (
                  <ExtensionCard
                    busy={busyId === extension.manifest.id}
                    extension={extension}
                    highlighted={highlightedId === extension.manifest.id}
                    mutable={mutable}
                    onOpen={() => onOpen(extension.manifest.id)}
                    setRef={(node) => setExtensionCardRef(cardRefs, extension.manifest.id, node)}
                    view="manage"
                  />
                )}
                renderItemPreview={(extension) => <Glyph name={extension.manifest.glyph} size={8} />}
                resetKey={`manage:${query}`}
                showCount
                title="Installed"
                visibleItemLimit={12}
              />
            </div>
          </>
        )}
        {!registry.loading && registry.errors.length && registry.extensions.length > 0 ? (
          <ExtensionRegistryErrors errors={registry.errors} />
        ) : null}
      </div>
    </div>
  );
}

function ExtensionCatalogSearch({ id, inputRef, onChange, onKeyDown, placeholder, value }: {
  id: string;
  inputRef: Ref<HTMLInputElement>;
  onChange: (value: string) => void;
  onKeyDown: (event: KeyboardEvent<HTMLInputElement>) => void;
  placeholder: string;
  value: string;
}) {
  return (
    <div className="sticky top-0 z-30 bg-token-main-surface-primary pt-1 pb-3 after:pointer-events-none after:absolute after:top-full after:right-0 after:left-0 after:h-8 after:bg-linear-to-b after:from-token-main-surface-primary after:to-transparent after:content-['']">
      <OcSearchField
        className="w-full"
        id={id}
        inputRef={inputRef}
        label={placeholder}
        onChange={onChange}
        onKeyDown={onKeyDown}
        placeholder={placeholder}
        value={value}
      />
    </div>
  );
}

function ExtensionReadOnlyNotice() {
  return (
    <div className="flex items-start gap-2 rounded-xl border border-token-border/40 bg-token-foreground/3 px-3 py-2.5 text-sm text-token-text-secondary">
      <Glyph className="mt-0.5 shrink-0" name="lock" size={14} />
      <span>Extensions are read-only in the Test profile. Switch to Administrator to install, remove, or configure them.</span>
    </div>
  );
}

function ExtensionRegistryErrors({ errors }: { errors: string[] }) {
  return (
    <div className="flex flex-col gap-2 border-t border-token-border-light pt-4">
      {errors.map((error) => (
        <div className="flex items-start gap-2 text-sm text-token-error-foreground" key={error}>
          <Glyph className="mt-0.5 shrink-0" name="warning" size={14} />
          <span className="[overflow-wrap:anywhere]">{error}</span>
        </div>
      ))}
    </div>
  );
}

function extensionKey(extension: ClientExtensionRecord): string {
  return extension.manifest.id;
}

function extensionName(extension: ClientExtensionRecord): string {
  return extension.manifest.name;
}

function setExtensionCardRef(
  refs: MutableRefObject<Map<string, HTMLDivElement>>,
  id: string,
  node: HTMLDivElement | null,
) {
  if (node) refs.current.set(id, node);
}

function ExtensionCard({
  busy,
  extension,
  highlighted,
  mutable,
  onInstall,
  onOpen,
  setRef,
  view = "browse",
}: {
  busy: boolean;
  extension: ClientExtensionRecord;
  highlighted: boolean;
  mutable: boolean;
  onInstall?: () => void;
  onOpen: () => void;
  setRef: (node: HTMLDivElement | null) => void;
  view?: "browse" | "manage";
}) {
  return (
    <div
      className={cx(
        "group flex cursor-interaction items-center justify-center gap-3 rounded-2xl border-none p-2 text-start text-base select-none outline-none hover:bg-text/5 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0",
        highlighted && "bg-text/5",
      )}
      onClick={onOpen}
      onKeyDown={(event) => {
        if (event.target !== event.currentTarget || !["Enter", " "].includes(event.key)) return;
        event.preventDefault();
        onOpen();
      }}
      ref={setRef}
      role="button"
      tabIndex={0}
    >
      <ExtensionVisual className={cx("border border-default", view === "browse" ? "size-9 bg-surface" : "size-10 bg-transparent")} extension={extension} size={19} />
      <div className="flex min-w-0 flex-1 flex-col justify-center gap-0.5">
        <div className="truncate font-medium text-default">{extension.manifest.name}</div>
        <div className="line-clamp-2 text-sm leading-relaxed text-text-description">{extension.manifest.description}</div>
        {extension.lifecycle.receiptState === "invalid" ? (
          <div className="mt-1 line-clamp-2 text-xs leading-4 text-token-error-foreground" title={extension.lifecycle.error}>
            {extension.lifecycle.error || "The installation receipt could not be verified."}
          </div>
        ) : null}
      </div>
      <div className="flex shrink-0 flex-col items-end gap-1">
        <ExtensionStateStatus busy={busy} extension={extension} />
        {!extension.installed && onInstall ? (
          <OcAnimatedActionSlot>
            <OcButton
              disabled={busy || !mutable || extension.lifecycle.receiptState === "invalid"}
              onClick={(event) => {
                event.stopPropagation();
                onInstall();
              }}
              size="xs"
              title={extension.lifecycle.receiptState === "invalid" ? extension.lifecycle.error || "The installation receipt could not be verified." : undefined}
              variant="secondary"
            >
              {extensionInstallActionLabel(extension, busy)}
            </OcButton>
          </OcAnimatedActionSlot>
        ) : null}
      </div>
    </div>
  );
}

function ExtensionStateStatus({ busy = false, extension }: { busy?: boolean; extension: ClientExtensionRecord }) {
  if (busy && !extension.installed) {
    return <OcStatus className="shrink-0" label={extensionNeedsUpdate(extension) ? "Updating" : "Installing"} />;
  }
  if (extension.lifecycle.receiptState === "invalid") {
    return <OcStatus className="shrink-0" label="Installation issue" tone="danger" />;
  }
  if (extension.installed) {
    return (
      <span className="flex shrink-0 flex-wrap justify-end gap-1">
        <OcStatus label="Installed" tone="success" />
        {!extension.configured ? <OcStatus label="Needs setup" tone="warning" /> : null}
      </span>
    );
  }
  if (extension.admission === "pending") {
    return <OcStatus className="shrink-0" label={extensionNeedsUpdate(extension) ? "Review update" : "Review required"} tone="warning" />;
  }
  if (extensionNeedsUpdate(extension)) {
    return <OcStatus className="shrink-0" label="Update available" tone="warning" />;
  }
  return <OcStatus className="shrink-0" label="Available" />;
}

function extensionNeedsUpdate(extension: ClientExtensionRecord): boolean {
  return extension.lifecycle.receiptState === "stale" && extension.lifecycle.desiredState === "installed";
}

function extensionInstallActionLabel(extension: ClientExtensionRecord, busy: boolean): string {
  if (busy) return extensionNeedsUpdate(extension) ? "Updating…" : "Installing…";
  if (extension.admission === "pending") return "Review";
  return extensionNeedsUpdate(extension) ? "Update" : "Install";
}

function ExtensionDetail({ busy, extension, mutable, onBack, onInstall, onSetup, onUninstall }: {
  busy: boolean;
  extension: ClientExtensionRecord;
  mutable: boolean;
  onBack: () => void;
  onInstall: () => void;
  onSetup: () => void;
  onUninstall: () => void;
}) {
  const sourceCount = (extension.manifest.sources?.length ?? 0) + (extension.surface?.document.sources?.length ?? 0);
  return (
    <div className="@container mx-auto flex w-full max-w-[var(--thread-content-max-width)] flex-col gap-[var(--detail-page-section-gap)] [--detail-page-inline-inset:var(--padding-row-x)] [--detail-page-section-gap:calc(var(--spacing)*6)]">
      <button className="flex w-fit cursor-interaction items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-secondary hover:bg-primary-ghost-hover hover:text-default" onClick={onBack} type="button">
        <Glyph name="arrowLeft" size={14} />
        <span>Extensions</span>
      </button>
      <header className="flex flex-col gap-4 px-[var(--detail-page-inline-inset,0px)]">
        <ExtensionVisual className="size-[60px] rounded-2xl border border-default bg-transparent" extension={extension} size={28} />
        <div className="flex min-w-0 flex-wrap items-start justify-between gap-4">
          <div className="flex min-w-0 flex-1 basis-64 flex-col gap-1.5">
            <h1 className="heading-base min-w-0 break-words text-default electron:heading-lg">{extension.manifest.name}</h1>
            <p className="text-base text-secondary">{extension.manifest.description}</p>
          </div>
          <div className="flex max-w-full shrink-0 items-center gap-2">
            <ExtensionStateStatus busy={busy} extension={extension} />
            {(extension.manifest.sources?.length ?? 0) > 0 ? (
              <OcButton
                disabled={busy || !mutable}
                onClick={onSetup}
                size="sm"
                variant={extension.installed && !extension.configured ? "primary" : "secondary"}
              >
                {extension.configured ? "Edit setup" : "Set up"}
              </OcButton>
            ) : null}
            {extension.installed ? (
              <OcButton disabled={busy || !mutable} onClick={onUninstall} size="sm" tone="danger" variant="ghost">Uninstall</OcButton>
            ) : (
              <OcButton
                disabled={busy || !mutable || extension.lifecycle.receiptState === "invalid"}
                onClick={onInstall}
                size="sm"
                title={extension.lifecycle.receiptState === "invalid" ? extension.lifecycle.error || "The installation receipt could not be verified." : undefined}
                variant="primary"
              >
                {extensionInstallActionLabel(extension, busy)}
              </OcButton>
            )}
          </div>
        </div>
      </header>
      {extension.lifecycle.receiptState === "invalid" ? (
        <div className="mx-2 rounded-xl border border-token-error-foreground/25 bg-token-error-background px-4 py-3 text-sm text-token-error-foreground" role="alert">
          <div className="font-medium">Installation issue</div>
          <div className="mt-1 whitespace-pre-wrap text-xs opacity-90">{extension.lifecycle.error || "The installation receipt could not be verified."}</div>
        </div>
      ) : null}
      <DetailSection title="About">
        <DetailRow label="Type" value={extensionCategoryLabel(extension.manifest.category)} />
        <DetailRow label="Adds" value={extensionDestinationLabel(extension.manifest.category)} />
        <DetailRow label="Sources" value={sourceCount ? `${sourceCount} local source${sourceCount === 1 ? "" : "s"}` : "No setup needed"} />
      </DetailSection>
      {(extension.manifest.sources?.length ?? 0) > 0 ? (
        <DetailSection title="Sources">
          {extension.manifest.sources?.map((source) => {
            const configuration = extension.sourceConfiguration[source.id];
            return (
              <DetailRow
                key={source.id}
                label={source.label}
                value={source.kind === "filesystem"
                  ? configuration?.path || (source.required ? "Required · not configured" : "Optional · not configured")
                  : source.kind === "credential"
                    ? extension.credentials[source.id]
                      ? `Saved · sent only to ${credentialHost(source.origin)}`
                      : source.required ? "Required · not configured" : "Optional · not configured"
                    : configuration?.device_id || `${source.device} · system default`}
                mono={source.kind === "filesystem" && Boolean(configuration?.path)}
              />
            );
          })}
        </DetailSection>
      ) : null}
      <DetailSection title="Information">
        <DetailRow label="Developer" value={extension.manifest.developer} />
        <DetailRow label="Version" value={extension.manifest.version} mono />
      </DetailSection>
    </div>
  );
}

function ExtensionSetupDialog({
  accessProfile,
  busy,
  extension,
  installAfterSave,
  onCancel,
  onCheckCredential,
  onRemoveCredential,
  onSave,
}: {
  accessProfile: OctopusClientAccessProfile;
  busy: boolean;
  extension: ClientExtensionRecord | null;
  installAfterSave: boolean;
  onCancel: () => void;
  onCheckCredential: (sourceId: string) => Promise<ClientExtensionCredentialCheck>;
  onRemoveCredential: (sourceId: string) => void;
  onSave: (sources: Record<string, ClientExtensionSourceConfiguration>, credentials: Record<string, string>) => void;
}) {
  const [draft, setDraft] = useState<Record<string, ClientExtensionSourceConfiguration>>({});
  const [secrets, setSecrets] = useState<Record<string, string>>({});
  const [checks, setChecks] = useState<Record<string, { pending: boolean; message: string; ok: boolean }>>({});
  const [devices, setDevices] = useState<Record<string, MediaDeviceInfo[]>>({});
  const [localError, setLocalError] = useState("");
  const extensionId = extension?.manifest.id ?? "";

  useEffect(() => {
    setSecrets({});
    setChecks({});
  }, [extensionId]);

  useEffect(() => {
    if (!extension) return;
    setDraft(Object.fromEntries(Object.entries(extension.sourceConfiguration).map(([id, value]) => [id, { ...value }])));
    setLocalError("");
    void loadDevices(extension).then(setDevices).catch((error) => setLocalError(error instanceof Error ? error.message : String(error)));
  }, [extension]);

  async function choosePath(sourceId: string, mode: "file" | "directory", label: string) {
    const select = window.octopusTransport?.selectClientDirectory;
    if (!select) {
      setLocalError("Source selection requires the desktop client.");
      return;
    }
    try {
      const result = await select({ mode, title: `Choose ${label}`, buttonLabel: "Choose" });
      if (!result.canceled) setDraft((current) => ({ ...current, [sourceId]: { ...current[sourceId], path: result.path } }));
      setLocalError("");
    } catch (error) {
      setLocalError(error instanceof Error ? error.message : String(error));
    }
  }

  async function requestDevice(sourceId: string, device: "camera" | "microphone") {
    try {
      if (device === "camera") await window.octopusTransport?.requestCameraAccess?.();
      else await window.octopusTransport?.requestMicrophoneAccess?.();
      if (extension) setDevices(await loadDevices(extension));
      setLocalError("");
      const candidates = (await navigator.mediaDevices.enumerateDevices()).filter((candidate) => candidate.kind === (device === "camera" ? "videoinput" : "audioinput"));
      if (candidates[0]) setDraft((current) => ({ ...current, [sourceId]: { ...current[sourceId], device_id: candidates[0].deviceId } }));
    } catch (error) {
      setLocalError(error instanceof Error ? error.message : String(error));
    }
  }

  async function checkCredential(sourceId: string, host: string) {
    setChecks((current) => ({ ...current, [sourceId]: { pending: true, message: "Checking…", ok: false } }));
    try {
      const result = await onCheckCredential(sourceId);
      const message = !result.checked
        ? "This source declares no check."
        : result.ok
          ? `${host} accepted the key.`
          : `${host} refused the key (HTTP ${result.status}).`;
      setChecks((current) => ({ ...current, [sourceId]: { pending: false, message, ok: result.checked && result.ok } }));
    } catch (error) {
      setChecks((current) => ({
        ...current,
        [sourceId]: { pending: false, message: error instanceof Error ? error.message : String(error), ok: false },
      }));
    }
  }

  const missingRequired = (extension?.manifest.sources ?? []).some((source) => {
    if (!source.required) return false;
    const configuration = draft[source.id];
    if (source.kind === "credential") return !extension?.credentials[source.id] && !secrets[source.id]?.trim();
    return source.kind === "filesystem" ? !isAbsoluteClientPath(configuration?.path ?? "") : !configuration?.device_id?.trim();
  });
  const invalidFilesystem = (extension?.manifest.sources ?? []).some((source) => (
    source.kind === "filesystem"
    && Boolean(draft[source.id]?.path?.trim())
    && !isAbsoluteClientPath(draft[source.id]?.path ?? "")
  ));
  return (
    <SetupDialogShell ariaLabel={extension ? `Set up ${extension.manifest.name}` : "Set up extension"} busy={busy} onClose={onCancel} open={Boolean(extension)} size="wide">
      {extension ? (
        <>
          <div className="flex items-start gap-4 px-6 pt-8 pb-6">
            <ExtensionVisual className="size-[60px] rounded-2xl" extension={extension} size={28} />
            <div className="min-w-0 flex-1">
              <h2 className="heading-lg text-token-foreground">Set up {extension.manifest.name}</h2>
              <p className="mt-1 text-sm leading-5 text-token-text-secondary">These sources stay on this device and are available only to {extension.manifest.name}.</p>
            </div>
            <OcIconButton disabled={busy} glyph="close" label="Close" onClick={onCancel} variant="ghost" />
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-6">
            <div className="flex flex-col gap-5">
              {extension.manifest.sources?.map((source) => source.kind === "filesystem" ? (
                <div className="flex flex-col gap-2" key={source.id}>
                  <div>
                    <div className="text-sm font-medium text-token-foreground">{source.label}{source.required ? " · Required" : ""}</div>
                    <div className="mt-0.5 text-sm text-token-description-foreground">{source.description}</div>
                  </div>
                  <div className="flex items-start gap-2">
                    <OcTextField
                      className="min-w-0 flex-1"
                      disabled={busy}
                      label={source.label}
                      error={draft[source.id]?.path?.trim() && !isAbsoluteClientPath(draft[source.id]?.path ?? "") ? "Use an absolute path." : undefined}
                      onChange={(event) => setDraft((current) => ({ ...current, [source.id]: { ...current[source.id], path: event.currentTarget.value } }))}
                      placeholder={source.selection === "directory" ? "/path/to/folder" : "/path/to/file"}
                      showLabel={false}
                      value={draft[source.id]?.path ?? ""}
                    />
                    <OcButton disabled={busy || accessProfile !== "live"} onClick={() => void choosePath(source.id, source.selection, source.label)} size="sm" variant="secondary">Choose</OcButton>
                  </div>
                </div>
              ) : source.kind === "credential" ? (
                <div className="flex flex-col gap-2" key={source.id}>
                  <div>
                    <div className="text-sm font-medium text-token-foreground">{source.label}{source.required ? " · Required" : ""}</div>
                    <div className="mt-0.5 text-sm text-token-description-foreground">{source.description}</div>
                  </div>
                  <div className="flex items-start gap-2">
                    <OcTextField
                      autoComplete="off"
                      className="min-w-0 flex-1"
                      disabled={busy}
                      label={source.label}
                      onChange={(event) => setSecrets((current) => ({ ...current, [source.id]: event.currentTarget.value }))}
                      placeholder={extension.credentials[source.id] ? "Saved · paste a new key to replace it" : "Paste key"}
                      showLabel={false}
                      spellCheck={false}
                      type="password"
                      value={secrets[source.id] ?? ""}
                    />
                    {extension.credentials[source.id] ? (
                      <>
                        {source.check_path ? (
                          <OcButton
                            disabled={busy || checks[source.id]?.pending || accessProfile !== "live"}
                            onClick={() => void checkCredential(source.id, credentialHost(source.origin))}
                            size="sm"
                            variant="secondary"
                          >
                            Check
                          </OcButton>
                        ) : null}
                        <OcButton
                          disabled={busy || accessProfile !== "live"}
                          onClick={() => onRemoveCredential(source.id)}
                          size="sm"
                          tone="danger"
                          variant="ghost"
                        >
                          Remove
                        </OcButton>
                      </>
                    ) : null}
                  </div>
                  {checks[source.id]?.message ? (
                    <div className={`text-sm ${checks[source.id]?.ok ? "text-token-description-foreground" : "text-token-error-foreground"}`}>
                      {checks[source.id]?.message}
                    </div>
                  ) : null}
                </div>
              ) : (
                <div className="flex flex-col gap-2" key={source.id}>
                  <div>
                    <div className="text-sm font-medium text-token-foreground">{source.label}{source.required ? " · Required" : ""}</div>
                    <div className="mt-0.5 text-sm text-token-description-foreground">{source.description}</div>
                  </div>
                  <div className="flex items-start gap-2">
                    <OcSelect
                      disabled={busy || !(devices[source.id]?.length)}
                      label={source.label}
                      onChange={(deviceId) => setDraft((current) => ({ ...current, [source.id]: { ...current[source.id], device_id: deviceId } }))}
                      options={(devices[source.id] ?? []).map((device, index) => ({ value: device.deviceId, label: device.label || `${source.device} ${index + 1}` }))}
                      placeholder={`Choose ${source.device}`}
                      showLabel={false}
                      stretch
                      value={draft[source.id]?.device_id ?? ""}
                    />
                    <OcButton disabled={busy || accessProfile !== "live"} onClick={() => void requestDevice(source.id, source.device)} size="sm" variant="secondary">Allow</OcButton>
                  </div>
                </div>
              ))}
              {localError ? <div className="text-sm text-token-error-foreground">{localError}</div> : null}
            </div>
          </div>
          <OcDialogFooter>
            <OcButton disabled={busy} onClick={onCancel} variant="ghost">Cancel</OcButton>
            <OcButton disabled={busy || accessProfile !== "live" || missingRequired || invalidFilesystem} onClick={() => onSave(draft, secrets)} variant="primary">{busy ? "Saving…" : installAfterSave ? "Save and install" : "Save"}</OcButton>
          </OcDialogFooter>
        </>
      ) : null}
    </SetupDialogShell>
  );
}

function ExtensionUninstallDialog({ busy, extension, onCancel, onConfirm }: {
  busy: boolean;
  extension: ClientExtensionRecord | null;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  return (
    <SetupDialogShell ariaLabel={extension ? `Uninstall ${extension.manifest.name}` : "Uninstall extension"} busy={busy} onClose={onCancel} open={Boolean(extension)} panelClassName="max-w-md">
      {extension ? (
        <>
          <div className="px-6 pt-8 pb-6">
            <h2 className="heading-lg text-token-foreground">Uninstall {extension.manifest.name}?</h2>
            <p className="mt-2 text-sm leading-5 text-token-text-secondary">This removes the extension from Octopus. Its setup is kept so you can install it again later.</p>
          </div>
          <OcDialogFooter>
            <OcButton disabled={busy} onClick={onCancel} variant="ghost">Cancel</OcButton>
            <OcButton disabled={busy} onClick={onConfirm} tone="danger" variant="secondary">{busy ? "Uninstalling…" : "Uninstall"}</OcButton>
          </OcDialogFooter>
        </>
      ) : null}
    </SetupDialogShell>
  );
}

function ExtensionInstallDialog({ busy, extension, onCancel, onConfirm }: {
  busy: boolean;
  extension: ClientExtensionRecord | null;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const invalid = extension?.lifecycle.receiptState === "invalid";
  const updating = extension ? extensionNeedsUpdate(extension) : false;
  return (
    <SetupDialogShell ariaLabel={extension ? `Install ${extension.manifest.name}` : "Install extension"} busy={busy} onClose={onCancel} open={Boolean(extension)} panelClassName="max-w-md">
      {extension ? (
        <>
          <div className="px-6 pt-8 pb-6">
            <h2 className="heading-lg text-token-foreground">
              {invalid
                ? `Installation issue with ${extension.manifest.name}`
                : extension.admission === "pending"
                  ? `Review ${extension.manifest.name}${updating ? " update" : ""}?`
                  : updating
                    ? `Update ${extension.manifest.name}?`
                    : `Install ${extension.manifest.name}?`}
            </h2>
            <p className="mt-2 text-sm leading-5 text-token-text-secondary">
              {invalid
                ? extension.lifecycle.error || "The installation receipt could not be verified."
                : extension.admission === "pending"
                ? "Octopus found a new local package revision. Review this exact version before installing it."
                : extension.manifest.category === "repair"
                  ? "This adds a guided repair that appears only when Octopus recognizes the matching problem and still waits for your confirmation."
                  : `This adds ${extensionDestinationLabel(extension.manifest.category).toLowerCase()} and uses only the sources shown in its setup.`}
            </p>
            {extension.admission === "pending" ? (
              <dl className="mt-5 grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-2 rounded-xl border border-token-border/50 bg-token-foreground/3 px-3 py-3 text-sm">
                <dt className="text-token-text-secondary">Version</dt><dd className="min-w-0 truncate font-mono text-xs text-token-foreground" title={extension.manifest.version}>{extension.manifest.version}</dd>
                <dt className="text-token-text-secondary">Package fingerprint</dt><dd className="min-w-0 truncate font-mono text-xs text-token-foreground" title={extension.sourceDigest}>{extension.sourceDigest}</dd>
                <dt className="text-token-text-secondary">Adds</dt><dd className="min-w-0 truncate text-token-foreground">{extensionDestinationLabel(extension.manifest.category)}</dd>
              </dl>
            ) : null}
          </div>
          <OcDialogFooter>
            <OcButton disabled={busy} onClick={onCancel} variant="ghost">{invalid ? "Close" : extension.admission === "pending" ? "Dismiss" : "Cancel"}</OcButton>
            <OcButton disabled={busy || invalid} onClick={onConfirm} variant="primary">
              {busy
                ? updating ? "Updating…" : "Installing…"
                : extension.admission === "pending"
                  ? updating ? "Review & update" : "Review & install"
                  : updating ? "Update" : "Install"}
            </OcButton>
          </OcDialogFooter>
        </>
      ) : null}
    </SetupDialogShell>
  );
}

function ExtensionVisual({ extension, className, size }: { extension: ClientExtensionRecord; className?: string; size: number }) {
  return (
    <span className={cx(
      "flex shrink-0 items-center justify-center overflow-hidden rounded-lg text-token-icon-foreground",
      className,
    )}>
      <Glyph name={extension.manifest.glyph} size={size} />
    </span>
  );
}

function DetailSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="group/section flex flex-col gap-4 [--sectioned-page-leading-inset:var(--detail-page-inline-inset,0px)]">
      <div className="flex items-center justify-between gap-3 border-b border-subtle pe-0.5 pb-2 text-default [padding-inline-start:var(--sectioned-page-leading-inset,0.5rem)]">
        <h2 className="flex min-h-7 items-center gap-1.5 text-lg leading-6 font-medium">{title}</h2>
      </div>
      <div className="flex flex-col gap-1 px-[var(--detail-page-inline-inset,0px)]">{children}</div>
    </section>
  );
}

function DetailRow({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="grid min-h-9 grid-cols-[minmax(7rem,0.35fr)_minmax(0,1fr)] items-start gap-4 rounded-lg px-2 py-1.5 hover:bg-token-foreground/3">
      <span className="text-sm text-token-text-secondary">{label}</span>
      <span className={cx("min-w-0 text-sm text-token-foreground [overflow-wrap:anywhere]", mono && "font-mono text-xs")}>{value}</span>
    </div>
  );
}

async function loadDevices(extension: ClientExtensionRecord): Promise<Record<string, MediaDeviceInfo[]>> {
  const deviceSources = (extension.manifest.sources ?? []).filter((source) => source.kind === "device");
  if (!deviceSources.length) return {};
  if (!navigator.mediaDevices?.enumerateDevices) throw new Error("Media devices are unavailable in this client.");
  const available = await navigator.mediaDevices.enumerateDevices();
  return Object.fromEntries(deviceSources.map((source) => [
    source.id,
    available.filter((device) => device.kind === (source.device === "camera" ? "videoinput" : "audioinput")),
  ]));
}

function rankExtensions(extensions: ClientExtensionRecord[], query: string): ClientExtensionRecord[] {
  const needle = normalizeSearch(query);
  if (!needle) return extensions;
  return extensions
    .flatMap((extension) => {
      const name = normalizeSearch(extension.manifest.name);
      const id = normalizeSearch(extension.manifest.id);
      const text = normalizeSearch(`${extension.manifest.description} ${extension.manifest.developer} ${CATEGORY_COPY[extension.manifest.category].title}`);
      const score = name === needle
        ? 0
        : name.startsWith(needle)
          ? 1
          : name.includes(needle)
            ? 2
            : id.includes(needle)
              ? 3
              : text.includes(needle)
                ? 4
                : -1;
      return score < 0 ? [] : [{ extension, score }];
    })
    .sort((left, right) => left.score - right.score || left.extension.manifest.name.localeCompare(right.extension.manifest.name))
    .map(({ extension }) => extension);
}

function normalizeSearch(value: string): string {
  return value.toLowerCase().replace(/[^\p{L}\p{N}]+/gu, " ").trim();
}

function isAbsoluteClientPath(value: string): boolean {
  const candidate = value.trim();
  return candidate.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(candidate) || /^\\\\[^\\]/.test(candidate);
}

function revealInstalledTab(
  extension: ClientExtensionRecord,
  hiddenSurfaceIds: readonly string[],
  setClientPreferences: (partial: { hiddenSurfaceIds: string[] }) => unknown,
) {
  if (extension.manifest.category !== "tab") return;
  const contribution = extension.manifest.contribution;
  if (contribution.kind === "builtin_action" || contribution.kind === "builtin_input") return;
  const surfaceId = contribution.kind === "builtin" ? contribution.surface_id : contribution.document.id;
  if (!hiddenSurfaceIds.includes(surfaceId)) return;
  setClientPreferences({ hiddenSurfaceIds: hiddenSurfaceIds.filter((id) => id !== surfaceId) });
}

function extensionCategoryLabel(category: ClientExtensionCategory): string {
  if (category === "widget") return "Widget";
  if (category === "tab") return "Tab or panel";
  if (category === "input") return "Input";
  return "Repair";
}

function extensionDestinationLabel(category: ClientExtensionCategory): string {
  if (category === "widget") return "A Home widget";
  if (category === "tab") return "A dedicated app space";
  if (category === "input") return "A way to control the client";
  return "A guided repair";
}

function credentialHost(origin: string): string {
  return new URL(origin).host;
}
