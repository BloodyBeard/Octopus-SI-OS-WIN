import "./icon-glyphs/chat";
import "./catalog/application-artwork.css";
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import type {
  OctopusApplicationLifecycleProgress,
  OctopusClientAccessProfile,
  OctopusClientSetupState,
} from "../electron";
import type { ClientExtensionActionStatus, ClientExtensionRecord } from "../extensions/types";
import {
  clientExtensionIncidentSnapshot,
  subscribeClientExtensionIncidents,
} from "../extensions/incidents";
import { useClientExtensions } from "../extensions/useClientExtensions";
import {
  formatProjectionError,
  hasProjectionBridge,
  mutateApplicationPrivateConfiguration,
  mutateOctopusOsWorkspace,
  onApplicationLifecycleProgress,
  readCurrentClientSetup,
  readProjection,
} from "../projectionClient";
import { useProjectionStructureGeneration } from "../projectionLive";
import {
  OcButton,
  OcEmptyState,
  OcIconButton,
  OcInlineProgress,
  OcSearchField,
  OcSegmented,
  OcStatus,
  OcSwitch,
  cx,
} from "../design-system/components";
import {
  CatalogInstalledRail,
  CatalogRouteTransition,
  CatalogSection,
} from "./catalog";
import { Glyph, type GlyphName } from "./IconGlyphs";
import { OctopusLoadingState } from "./OctopusLoadingLogo";
import { SetupDialogShell } from "./SetupDialogShell";
import { useAppErrorToast } from "../toasts";

type ApplicationCategory = "provider" | "connector" | "plugin" | "artifact";

type ApplicationProjectState = {
  ee_id: string;
  is_root: boolean;
  policy_allowed: boolean;
  ceiling_allowed: boolean;
  selected_by_default: boolean;
  record_digest: string;
};

type ApplicationConfigurationField = {
  interface_id: string;
  name: string;
  label: string;
  kind: "text" | "url" | "secret";
  required: boolean;
  configured: boolean;
  private_saved: boolean;
  source: "private" | "environment" | "both" | "conflict" | "missing" | "unknown";
};

type ApplicationConfiguration = {
  status: "not_installed" | "configured" | "required" | "unknown" | "issue";
  fields: ApplicationConfigurationField[];
  documents: Array<{ interface_id: string; status: string; digest: string }>;
  error: string;
};

type ApplicationRecord = {
  id: string;
  name: string;
  description: string;
  category: ApplicationCategory;
  developer: string;
  version: string;
  icon: string;
  logo: string;
  logo_dark: string;
  logo_data_url: string;
  logo_dark_data_url: string;
  interfaces: string[];
  artifact_bindings: Array<{ artifact_id: string; label?: string }>;
  installed: boolean;
  materialized: boolean;
  partially_materialized: boolean;
  valid: boolean;
  missing_interface_ids: string[];
  descriptor_path: string;
  manifest_path: string;
  manifest_digest: string;
  package_id: string;
  installed_digest: string;
  source_available: boolean;
  source_kind: "local" | "none";
  source_digest: string;
  install_packages: Array<{ id: string; name: string; source_digest: string; installed: boolean; receipt_state: string }>;
  source_file_count: number;
  receipt_state: string;
  receipt_digest: string;
  managed: boolean;
  install_block_reason: string;
  removal_allowed: boolean;
  removal_block_reason: string;
  connected_run_ids: string[];
  partially_connected_run_ids?: string[];
  project_states: ApplicationProjectState[];
  configuration?: ApplicationConfiguration;
};

type ApplicationProject = {
  ee_id: string;
  name: string;
  icon: string;
  color: string;
  goal: string;
  parent_ee_id: string;
  record_digest: string;
};

type ApplicationsProjection = {
  status?: string;
  root_ee_id?: string;
  applications?: ApplicationRecord[];
  projects?: ApplicationProject[];
  errors?: unknown[];
};

type ApplicationPackComponent = {
  package_id: string;
  selection: "included" | "recommended";
  reason: string;
  product_kind: "application" | "driver" | "artifact";
  product_id: string;
  name: string;
  available: boolean;
  installed: boolean;
  materialized: boolean;
  partially_materialized: boolean;
  receipt_state: string;
  source_digest: string;
  error: string;
};

type ApplicationPackExtension = {
  extension_id: string;
  selection: "recommended";
  reason: string;
};

type ApplicationPackRecord = {
  id: string;
  manifest_digest: string;
  name: string;
  description: string;
  version: string;
  components: ApplicationPackComponent[];
  extensions: ApplicationPackExtension[];
  required_components_materialized: boolean;
  workspace_setup?: {
    selection: "included";
    home: {
      provider_interface_id: string;
      worker_interface_id: string;
      context_artifact_bindings: Array<{ id: string }>;
      selected_worker_environment_kinds: string[];
    };
    project_defaults: Array<{
      package_id: string;
      default_for_new_tasks: boolean;
    }>;
  };
};

type LocalPackagesProjection = {
  packs?: ApplicationPackRecord[];
  errors?: unknown[];
};

type SetupDraft = Record<string, { available: boolean; selected: boolean }>;
type ConfigurationDraft = Record<string, { value: string; clear: boolean }>;

type ApplicationLifecycleOperation = {
  operationId: string;
  operation: "install" | "remove" | "repair";
  phase: OctopusApplicationLifecycleProgress["phase"] | "failed";
  percent: number;
  error: string;
};

type ApplicationPackLifecycleOperation = Omit<ApplicationLifecycleOperation, "operation"> & {
  operation: "install";
};

type ApplicationCatalogView = "browse" | "manage";

const CATEGORY_ORDER: ApplicationCategory[] = ["provider", "connector", "plugin", "artifact"];
const SHA256_DIGEST = /^[a-f0-9]{64}$/;
const CATEGORY_COPY: Record<ApplicationCategory, { title: string; description: string }> = {
  provider: { title: "AI Providers", description: "Models for Home and tasks. Each provider connects through its own sign-in, API key, or local service." },
  connector: { title: "Connectors", description: "Services and triggers that bring messages into a task and carry its replies back out." },
  plugin: { title: "Plugins", description: "Tools and guides a task picks up when it needs them, such as MCP servers, memory and image generation." },
  artifact: { title: "Context", description: "Knowledge packages you can attach to new tasks." },
};

export function ApplicationsView({
  accessProfile,
  searchFocusTick,
}: {
  accessProfile: OctopusClientAccessProfile;
  searchFocusTick: number;
}) {
  const generation = useProjectionStructureGeneration();
  const extensionRegistry = useClientExtensions(accessProfile);
  const extensionIncidents = useSyncExternalStore(
    subscribeClientExtensionIncidents,
    clientExtensionIncidentSnapshot,
  );
  const browseSearchRef = useRef<HTMLInputElement>(null);
  const manageSearchRef = useRef<HTMLInputElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const scrollOffsetRef = useRef(0);
  const browseCardRefs = useRef(new Map<string, HTMLElement>());
  const browsePackRefs = useRef(new Map<string, HTMLElement>());
  const manageCardRefs = useRef(new Map<string, HTMLElement>());
  const detailOriginRef = useRef<HTMLElement | null>(null);
  const [applications, setApplications] = useState<ApplicationRecord[]>([]);
  const [applicationPacks, setApplicationPacks] = useState<ApplicationPackRecord[]>([]);
  const [projects, setProjects] = useState<ApplicationProject[]>([]);
  const [rootEeId, setRootEeId] = useState("");
  const [loaded, setLoaded] = useState(false);
  const [packsLoaded, setPacksLoaded] = useState(false);
  const [error, setError] = useState("");
  const [packError, setPackError] = useState("");
  const [clientSetup, setClientSetup] = useState<OctopusClientSetupState | null>(null);
  const [clientSetupError, setClientSetupError] = useState("");
  const [browseQuery, setBrowseQuery] = useState("");
  const [manageQuery, setManageQuery] = useState("");
  const [catalogView, setCatalogView] = useState<ApplicationCatalogView>("browse");
  const [detailReturnView, setDetailReturnView] = useState<ApplicationCatalogView>("browse");
  const [routeDirection, setRouteDirection] = useState<"back" | "forward">("forward");
  const [manageCategory, setManageCategory] = useState<ApplicationCategory>("connector");
  const [selectedId, setSelectedId] = useState("");
  const [selectedPackId, setSelectedPackId] = useState("");
  const [highlightedId, setHighlightedId] = useState("");
  const [packRecommendedSelections, setPackRecommendedSelections] = useState<Record<string, string[]>>({});
  const [setupId, setSetupId] = useState("");
  const [setupDraft, setSetupDraft] = useState<SetupDraft>({});
  const [configurationDraft, setConfigurationDraft] = useState<ConfigurationDraft>({});
  const [saving, setSaving] = useState(false);
  const [reloadTick, setReloadTick] = useState(0);
  const [removeId, setRemoveId] = useState("");
  const [installReview, setInstallReview] = useState<ApplicationRecord | null>(null);
  const [lifecycleOperations, setLifecycleOperations] = useState<Record<string, ApplicationLifecycleOperation>>({});
  const [packLifecycleOperations, setPackLifecycleOperations] = useState<Record<string, ApplicationPackLifecycleOperation>>({});
  useAppErrorToast(error || packError || clientSetupError);

  useEffect(() => {
    if (!hasProjectionBridge()) {
      setLoaded(true);
      return;
    }
    let cancelled = false;
    setLoaded(false);
    readProjection<ApplicationsProjection>("projection.operator.os.applications")
      .then((response) => {
        if (cancelled) return;
        setApplications(arrayRecords(response.applications));
        setProjects(arrayRecords(response.projects));
        setRootEeId(typeof response.root_ee_id === "string" ? response.root_ee_id : "");
        setError(arrayStrings(response.errors).join("\n"));
        setLoaded(true);
      })
      .catch((failure) => {
        if (cancelled) return;
        setError(formatProjectionError(failure));
        setLoaded(true);
      });
    return () => { cancelled = true; };
  }, [accessProfile, generation, reloadTick]);

  useEffect(() => {
    if (accessProfile !== "live" || !window.octopusTransport?.clientSetupReadCurrent) {
      setClientSetup(null);
      setClientSetupError("");
      return;
    }
    let cancelled = false;
    readCurrentClientSetup()
      .then((setup) => {
        if (cancelled) return;
        setClientSetup(setup);
        setClientSetupError("");
      })
      .catch(() => {
        if (cancelled) return;
        setClientSetup(null);
        setClientSetupError("Application Pack setup status could not be verified.");
      });
    return () => { cancelled = true; };
  }, [accessProfile, generation, reloadTick]);

  useEffect(() => {
    if (!hasProjectionBridge()) {
      setPacksLoaded(true);
      return;
    }
    let cancelled = false;
    setPacksLoaded(false);
    readProjection<LocalPackagesProjection>("projection.operator.os.local_packages")
      .then((response) => {
        if (cancelled) return;
        setApplicationPacks(arrayRecords(response.packs));
        setPackError(arrayStrings(response.errors).length ? "One or more local Application Packs could not be verified." : "");
        setPacksLoaded(true);
      })
      .catch(() => {
        if (cancelled) return;
        setApplicationPacks([]);
        setPackError("Application Packs could not be loaded.");
        setPacksLoaded(true);
      });
    return () => { cancelled = true; };
  }, [accessProfile, generation, reloadTick]);

  useEffect(() => onApplicationLifecycleProgress((progress) => {
    const applicationId = applications.find((application) => application.package_id === progress.packageId)?.id
      ?? progress.applicationId;
    setLifecycleOperations((current) => {
      const active = current[applicationId];
      if (!active || active.operationId !== progress.operationId || active.operation !== progress.operation) return current;
      return {
        ...current,
        [applicationId]: {
          ...active,
          phase: progress.phase,
          percent: progress.percent,
          error: "",
        },
      };
    });
    setPackLifecycleOperations((current) => {
      const matching = Object.entries(current).find(([, operation]) => operation.operationId === progress.operationId);
      if (!matching || progress.operation !== "install") return current;
      const [packId, operation] = matching;
      return {
        ...current,
        [packId]: {
          ...operation,
          phase: progress.phase,
          percent: progress.percent,
          error: "",
        },
      };
    });
  }), [applications]);

  useEffect(() => {
    if (searchFocusTick <= 0) return;
    setSelectedId("");
    setSelectedPackId("");
    setRouteDirection("back");
    setCatalogView("browse");
    window.requestAnimationFrame(() => browseSearchRef.current?.focus());
  }, [searchFocusTick]);

  const visible = useMemo(() => {
    const needle = browseQuery.trim().toLowerCase();
    return applications.filter((application) => !needle || applicationSearchText(application).includes(needle));
  }, [applications, browseQuery]);
  const visiblePacks = useMemo(() => {
    const needle = browseQuery.trim().toLowerCase();
    return applicationPacks.filter((pack) => !needle || applicationPackSearchText(pack).includes(needle));
  }, [applicationPacks, browseQuery]);
  const selected = applications.find((application) => application.id === selectedId) ?? null;
  const selectedPack = applicationPacks.find((pack) => pack.id === selectedPackId) ?? null;
  const setupApplication = applications.find((application) => application.id === setupId) ?? null;
  const removeApplication = applications.find((application) => application.id === removeId) ?? null;
  const groups = CATEGORY_ORDER.flatMap((category) => {
    const rows = visible.filter((application) => application.category === category);
    return rows.length ? [{ category, rows }] : [];
  });
  const manageNeedle = manageQuery.trim().toLowerCase();
  const manageApplications = applications.filter((application) => (
    application.category === manageCategory
    && (!manageNeedle || applicationSearchText(application).includes(manageNeedle))
  ));
  const setupStatusByInterface = useMemo(() => {
    const statuses = new Map(extensionRegistry.actionStatuses.map((status) => [status.interfaceId, status]));
    for (const incident of extensionIncidents) {
      if (incident.operation.status === "succeeded") continue;
      const status = statuses.get(incident.interfaceId);
      if (!status) continue;
      statuses.set(incident.interfaceId, {
        ...status,
        state: "sign_in_required",
        code: incident.code,
        message: incident.message,
      });
    }
    return statuses;
  }, [extensionIncidents, extensionRegistry.actionStatuses]);
  const extensionById = useMemo(() => new Map(
    extensionRegistry.extensions.map((extension) => [extension.manifest.id, extension]),
  ), [extensionRegistry.extensions]);
  function onSearchKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    const keyboardIds = catalogView === "manage"
      ? manageApplications.map((application) => application.id).filter((id) => manageCardRefs.current.has(id))
      : [
          ...visiblePacks.map((pack) => applicationPackKey(pack.id)).filter((id) => browsePackRefs.current.has(id)),
          ...groups.flatMap((group) => group.rows).map((application) => application.id).filter((id) => browseCardRefs.current.has(id)),
        ];
    if (!keyboardIds.length || !["ArrowDown", "ArrowUp", "Enter"].includes(event.key)) return;
    event.preventDefault();
    const current = Math.max(0, keyboardIds.indexOf(highlightedId));
    const nextIndex = event.key === "ArrowDown"
      ? (current + (highlightedId ? 1 : 0)) % keyboardIds.length
      : event.key === "ArrowUp"
        ? (current - 1 + keyboardIds.length) % keyboardIds.length
        : current;
    const nextId = highlightedId && event.key === "Enter" ? highlightedId : keyboardIds[nextIndex];
    if (event.key === "Enter") {
      if (catalogView === "browse" && nextId.startsWith("pack:")) openApplicationPack(nextId.slice(5));
      else openApplication(nextId, catalogView);
      return;
    }
    setHighlightedId(nextId);
    (catalogView === "manage"
      ? manageCardRefs.current.get(nextId)
      : nextId.startsWith("pack:")
        ? browsePackRefs.current.get(nextId)
        : browseCardRefs.current.get(nextId)
    )?.scrollIntoView({ block: "nearest" });
  }

  function openSetup(application: ApplicationRecord) {
    if (!application.installed || !application.valid) return;
    setSetupDraft(Object.fromEntries(application.project_states.map((state) => [
      state.ee_id,
      { available: state.policy_allowed, selected: state.selected_by_default },
    ])));
    setConfigurationDraft(Object.fromEntries(applicationConfiguration(application).fields.map((field) => [
      configurationFieldKey(field),
      { value: "", clear: false },
    ])));
    setSetupId(application.id);
  }

  function openApplication(id: string, returnView: ApplicationCatalogView = catalogView) {
    scrollOffsetRef.current = scrollRef.current?.scrollTop ?? 0;
    detailOriginRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setDetailReturnView(returnView);
    setRouteDirection("forward");
    setSelectedPackId("");
    setSelectedId(id);
  }

  function openApplicationPack(id: string) {
    scrollOffsetRef.current = scrollRef.current?.scrollTop ?? 0;
    detailOriginRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setDetailReturnView("browse");
    setRouteDirection("forward");
    setSelectedId("");
    setSelectedPackId(id);
  }

  function closeApplication() {
    setRouteDirection("back");
    setSelectedId("");
    window.requestAnimationFrame(() => {
      if (scrollRef.current) scrollRef.current.scrollTop = scrollOffsetRef.current;
      if (detailOriginRef.current?.isConnected) {
        detailOriginRef.current.focus();
        return;
      }
      const refs = detailReturnView === "manage" ? manageCardRefs : browseCardRefs;
      const card = refs.current.get(selectedId);
      if (card) {
        card.querySelector<HTMLButtonElement>("[data-application-open]")?.focus();
        return;
      }
      (detailReturnView === "manage" ? manageSearchRef : browseSearchRef).current?.focus();
    });
  }

  function closeApplicationPack() {
    const closingPackId = selectedPackId;
    setRouteDirection("back");
    setSelectedPackId("");
    window.requestAnimationFrame(() => {
      if (scrollRef.current) scrollRef.current.scrollTop = scrollOffsetRef.current;
      if (detailOriginRef.current?.isConnected) {
        detailOriginRef.current.focus();
        return;
      }
      browsePackRefs.current.get(applicationPackKey(closingPackId))
        ?.querySelector<HTMLButtonElement>("[data-application-pack-open]")
        ?.focus();
    });
  }

  function openManage() {
    setHighlightedId("");
    setRouteDirection("forward");
    setCatalogView("manage");
    window.requestAnimationFrame(() => manageSearchRef.current?.focus());
  }

  function closeManage() {
    setHighlightedId("");
    setRouteDirection("back");
    setCatalogView("browse");
    window.requestAnimationFrame(() => document.getElementById("applications-browse-manage")?.focus());
  }

  async function saveConfiguration(interfaceId: string) {
    if (!setupApplication || accessProfile !== "live") return;
    const configuration = applicationConfiguration(setupApplication);
    const document = configuration.documents.find((candidate) => candidate.interface_id === interfaceId);
    if (!document) return;
    const fields = configuration.fields.filter((field) => field.interface_id === interfaceId);
    const updates = Object.fromEntries(fields.flatMap((field) => {
      const next = configurationDraft[configurationFieldKey(field)];
      return next && !next.clear && next.value.length > 0 ? [[field.name, next.value]] : [];
    }));
    const clears = fields.flatMap((field) => (
      configurationDraft[configurationFieldKey(field)]?.clear ? [field.name] : []
    ));
    if (Object.keys(updates).length === 0 && clears.length === 0) return;
    setSaving(true);
    try {
      await mutateApplicationPrivateConfiguration({
        applicationId: setupApplication.id,
        interfaceId,
        expectedDigest: document.status === "valid" && document.digest ? document.digest : null,
        updates,
        clears,
      });
      setConfigurationDraft((current) => ({
        ...current,
        ...Object.fromEntries(fields.map((field) => [configurationFieldKey(field), { value: "", clear: false }])),
      }));
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setError(formatProjectionError(failure));
    } finally {
      setSaving(false);
    }
  }

  async function saveSetup() {
    if (!setupApplication) return;
    if (accessProfile !== "live") {
      setApplications((current) => current.map((application) => application.id !== setupApplication.id
        ? application
        : {
            ...application,
            project_states: application.project_states.map((state) => {
              const next = setupDraft[state.ee_id];
              return next ? {
                ...state,
                policy_allowed: next.available,
                selected_by_default: next.available && next.selected,
              } : state;
            }),
          }));
      setSetupId("");
      return;
    }
    setSaving(true);
    try {
      const changedProjects = setupApplication.project_states.flatMap((state) => {
        const next = setupDraft[state.ee_id];
        if (!next || (
          next.available === state.policy_allowed
          && next.selected === state.selected_by_default
        )) return [];
        return [{
          eeId: state.ee_id,
          available: next.available,
          selectedByDefault: next.selected,
          expectedDigest: state.record_digest,
        }];
      });
      if (changedProjects.length) {
        await mutateOctopusOsWorkspace({
          action: "configure_application",
          applicationId: setupApplication.id,
          projects: changedProjects,
        });
      }
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setError(formatProjectionError(failure));
    } finally {
      setSaving(false);
    }
  }

  async function installApplication(application: ApplicationRecord) {
    if (accessProfile !== "live" || application.installed || !application.source_digest || application.install_block_reason) return;
    const operationId = crypto.randomUUID();
    setLifecycleOperations((current) => ({
      ...current,
      [application.id]: { operationId, operation: "install", phase: "preflight", percent: 0, error: "" },
    }));
    try {
      await mutateOctopusOsWorkspace({
        action: "install_package",
        packageIds: application.install_packages.map((entry) => entry.id),
        expectedSourceDigests: Object.fromEntries(application.install_packages.map((entry) => [entry.id, entry.source_digest])),
        operationId,
      });
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: { operationId, operation: "install", phase: "complete", percent: 100, error: "" },
      }));
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: {
          operationId,
          operation: "install",
          phase: "failed",
          percent: current[application.id]?.percent ?? 0,
          error: formatProjectionError(failure),
        },
      }));
    }
  }

  async function repairApplication(application: ApplicationRecord) {
    const expectation = applicationRepairExpectation(application);
    if (accessProfile !== "live" || !expectation) return;
    const operationId = crypto.randomUUID();
    setLifecycleOperations((current) => ({
      ...current,
      [application.id]: { operationId, operation: "repair", phase: "preflight", percent: 0, error: "" },
    }));
    try {
      await mutateOctopusOsWorkspace({
        action: "repair_package",
        packageIds: [application.package_id],
        expectedRepairs: { [application.package_id]: expectation },
        operationId,
      });
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: { operationId, operation: "repair", phase: "complete", percent: 100, error: "" },
      }));
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: {
          operationId,
          operation: "repair",
          phase: "failed",
          percent: current[application.id]?.percent ?? 0,
          error: formatProjectionError(failure),
        },
      }));
    }
  }

  function installOrRepairApplication(application: ApplicationRecord) {
    if (applicationRepairExpectation(application)) return repairApplication(application);
    if (application.install_packages.length > 1) setInstallReview(application);
    else return installApplication(application);
  }

  async function removeInstalledApplication(application: ApplicationRecord) {
    if (accessProfile !== "live" || !application.installed || !application.installed_digest || !application.removal_allowed) return;
    const operationId = crypto.randomUUID();
    setRemoveId("");
    setLifecycleOperations((current) => ({
      ...current,
      [application.id]: { operationId, operation: "remove", phase: "preflight", percent: 0, error: "" },
    }));
    try {
      await mutateOctopusOsWorkspace({
        action: "remove_application",
        applicationId: application.id,
        expectedInstalledDigest: application.installed_digest,
        operationId,
      });
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: { operationId, operation: "remove", phase: "complete", percent: 100, error: "" },
      }));
      if (selectedId === application.id) closeApplication();
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setLifecycleOperations((current) => ({
        ...current,
        [application.id]: {
          operationId,
          operation: "remove",
          phase: "failed",
          percent: current[application.id]?.percent ?? 0,
          error: formatProjectionError(failure),
        },
      }));
    }
  }

  async function applyApplicationPack(pack: ApplicationPackRecord) {
    const selectedRecommended = selectedPackRecommendations(pack, packRecommendedSelections[pack.id]);
    if (accessProfile !== "live" || applicationPackInstallBlockReason(pack, selectedRecommended)) return;
    const operationId = crypto.randomUUID();
    const packageIds = pack.components.flatMap((component) => (
      component.selection === "included" || selectedRecommended.includes(component.package_id)
        ? [component.package_id]
        : []
    ));
    const expectedSourceDigests = Object.fromEntries(packageIds.map((packageId) => {
      const component = pack.components.find((candidate) => candidate.package_id === packageId);
      if (!component?.source_digest) throw new Error(`${pack.name} has an unavailable component.`);
      return [packageId, component.source_digest];
    }));
    setPackLifecycleOperations((current) => ({
      ...current,
      [pack.id]: { operationId, operation: "install", phase: "preflight", percent: 0, error: "" },
    }));
    try {
      await mutateOctopusOsWorkspace({
        action: "apply_application_pack",
        packId: pack.id,
        selectedRecommended,
        expectedSourceDigests,
        operationId,
      });
      setPackLifecycleOperations((current) => ({
        ...current,
        [pack.id]: { operationId, operation: "install", phase: "complete", percent: 100, error: "" },
      }));
      setReloadTick((current) => current + 1);
    } catch (failure) {
      setPackLifecycleOperations((current) => ({
        ...current,
        [pack.id]: {
          operationId,
          operation: "install",
          phase: "failed",
          percent: current[pack.id]?.percent ?? 0,
          error: formatProjectionError(failure),
        },
      }));
    }
  }

  if (!hasProjectionBridge()) return <SurfaceState title="Applications are unavailable" detail="Start Octopus to manage installed applications." />;
  const detailOpen = Boolean(selected || selectedPack);
  return (
    <>
      <div className="relative h-full min-h-0">
        <div aria-hidden={detailOpen || undefined} className={cx("h-full min-h-0", detailOpen && "pointer-events-none invisible absolute inset-0")}>
          <CatalogRouteTransition className="h-full min-h-0" direction={routeDirection} viewKey={`applications-${catalogView}`}>
            {!loaded || catalogView === "browse" && !packsLoaded ? (
              <OctopusLoadingState className="surface-content-loading-state" label="Loading applications" />
            ) : <section className="flex h-full min-h-0 flex-col text-base select-none">
              <header className="surface-titlebar-spacer draggable flex h-toolbar shrink-0 items-center justify-between gap-2 px-panel">
                {catalogView === "manage" ? (
                  <button className="no-drag cursor-interaction flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-token-text-secondary hover:bg-token-list-hover-background hover:text-token-foreground focus:outline-none" onClick={closeManage} type="button">
                    <span>Applications</span>
                    <span className="text-token-input-placeholder-foreground">/</span>
                    <span className="text-token-foreground">Manage</span>
                  </button>
                ) : (
                  <span className="no-drag flex min-w-0 items-center rounded-lg px-2 py-1 text-sm text-token-foreground">Applications</span>
                )}
                <div className="no-drag flex min-w-0 items-center justify-end gap-2">
                  {catalogView === "browse" ? (
                    <OcIconButton glyph="settings" id="applications-browse-manage" label="Manage applications" onClick={openManage} size="sm" variant="ghost" />
                  ) : null}
                </div>
              </header>
              <div className="plugins-surface-scroll relative h-full min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]" ref={scrollRef}>
                <div className="flex min-h-full w-full flex-col">
                  {catalogView === "browse" ? (
                    <>
                      <div className="mx-auto w-full max-w-[var(--thread-content-max-width)] px-panel pt-panel">
                        <div className="flex items-start justify-between gap-4 px-2">
                          <div className="flex min-w-0 flex-col gap-2">
                            <h1 className="heading-xl font-normal text-default">Applications</h1>
                            <div className="text-lg leading-6 text-secondary">Connect Octopus to the tools and context you use</div>
                          </div>
                        </div>
                      </div>
                      <div className="sticky top-0 z-30 bg-surface after:pointer-events-none after:absolute after:top-full after:right-0 after:left-0 after:h-8 after:bg-linear-to-b after:from-surface after:to-transparent after:content-['']">
                        <div className="mx-auto flex w-full max-w-[var(--thread-content-max-width)] items-center gap-2 px-panel pt-panel pb-2">
                          <OcSearchField autoFocus className="min-w-0 flex-1" id="applications-page-search" inputRef={browseSearchRef} label="Search applications" onChange={(value) => {
                            setBrowseQuery(value);
                            setHighlightedId("");
                          }} onKeyDown={onSearchKeyDown} placeholder="Search applications" value={browseQuery} />
                        </div>
                      </div>
                    </>
                  ) : null}
                  <div className={cx("mx-auto flex min-h-0 w-full max-w-[var(--thread-content-max-width)] flex-1 flex-col px-panel pb-panel", catalogView === "manage" ? "gap-3 pt-panel" : "gap-8 pt-5")}>
                    {catalogView === "manage" ? (
                      <div className="flex flex-wrap items-center justify-between gap-3">
                        <OcSegmented label="Manage applications" onChange={(category) => {
                          setManageCategory(category);
                          setHighlightedId("");
                        }} options={CATEGORY_ORDER.map((category) => ({
                          value: category,
                          label: <>{CATEGORY_COPY[category].title}<span className="ms-0.5 text-tertiary">{applications.filter((application) => application.category === category).length}</span></>,
                        }))} size="sm" value={manageCategory} />
                        <div className="w-56 min-w-0 max-w-full">
                          <OcSearchField className="shadow-none" id="applications-page-manage-search" inputRef={manageSearchRef} label="Search applications" onChange={(value) => {
                            setManageQuery(value);
                            setHighlightedId("");
                          }} onKeyDown={onSearchKeyDown} placeholder="Search" value={manageQuery} />
                        </div>
                      </div>
                    ) : null}
                    {loaded && error && applications.length ? <ApplicationProjectionIssue detail={error} /> : null}
                    {packsLoaded && packError && applicationPacks.length ? <ApplicationProjectionIssue detail={packError} /> : null}
                    {error && applications.length === 0 && (catalogView === "manage" || packError && applicationPacks.length === 0) ? (
                      <SurfaceState compact title="Applications could not be projected" detail={[error, packError].filter(Boolean).join("\n")} actionLabel="Retry" onAction={() => setReloadTick((current) => current + 1)} />
                    ) : applications.length === 0 && (catalogView === "manage" || applicationPacks.length === 0) ? (
                      <SurfaceState compact title="No application packages found" detail="No verified source or installed interface package is available." actionLabel="Refresh catalog" onAction={() => setReloadTick((current) => current + 1)} />
                    ) : (catalogView === "browse" && visible.length === 0 && visiblePacks.length === 0) || (catalogView === "manage" && manageApplications.length === 0 && manageNeedle) ? (
                      <SurfaceState compact title="No applications found" />
                    ) : catalogView === "manage" ? (
                      <>
                        {manageApplications.length ? (
                          <ManageApplicationsList accessProfile={accessProfile} applicationRefs={manageCardRefs} applications={manageApplications} highlightedId={highlightedId} lifecycleOperations={lifecycleOperations} onInstall={(application) => void installOrRepairApplication(application)} onOpen={(id) => openApplication(id, "manage")} onRemove={(application) => setRemoveId(application.id)} onSetup={openSetup} setupStatusByInterface={setupStatusByInterface} />
                        ) : <SurfaceState compact title={`No ${CATEGORY_COPY[manageCategory].title.toLowerCase()} found`} />}
                      </>
                    ) : (
                      <>
                        {visiblePacks.length ? (
                          <CatalogSection
                            description="Install a reviewed set of applications, local capabilities, and context"
                            getItemKey={(pack) => pack.id}
                            getItemLabel={(pack) => pack.name}
                            items={visiblePacks}
                            renderCard={(pack) => (
                              <ApplicationPackCard
                                accessProfile={accessProfile}
                                clientSetup={clientSetup}
                                clientSetupError={clientSetupError}
                                highlighted={highlightedId === applicationPackKey(pack.id)}
                                lifecycle={packLifecycleOperations[pack.id]}
                                onApply={() => void applyApplicationPack(pack)}
                                onOpen={() => openApplicationPack(pack.id)}
                                pack={pack}
                                setRef={(node) => {
                                  const key = applicationPackKey(pack.id);
                                  if (node) browsePackRefs.current.set(key, node);
                                  else browsePackRefs.current.delete(key);
                                }}
                                setupStatus={applicationPackSetupStatus(pack, setupStatusByInterface)}
                              />
                            )}
                            resetKey={`packs:${browseQuery}`}
                            showCount={Boolean(browseQuery.trim())}
                            title="Application Packs"
                            visibleItemLimit={3}
                          />
                        ) : null}
                        <CatalogInstalledRail
                          emptyLabel="No applications installed yet"
                          getItemKey={(application) => application.id}
                          getItemLabel={(application) => application.name}
                          items={applications.filter((application) => application.installed)}
                          manageLabel="Manage applications"
                          onManage={openManage}
                          onOpen={(application) => openApplication(application.id, "browse")}
                          renderIcon={(application, size) => <ApplicationLogo application={application} className="size-full" logoSize={size === "mini" ? 8 : 18} />}
                        />
                        {groups.map(({ category, rows }) => (
                          <CatalogSection
                            description={CATEGORY_COPY[category].description}
                            getItemKey={(application) => application.id}
                            getItemLabel={(application) => application.name}
                            items={rows}
                            key={category}
                            renderCard={(application) => (
                              <ApplicationCard accessProfile={accessProfile} application={application} highlighted={highlightedId === application.id} lifecycle={lifecycleOperations[application.id]} onInstall={() => void installOrRepairApplication(application)} onOpen={(id) => openApplication(id, "browse")} onRemove={() => setRemoveId(application.id)} onSetup={() => openSetup(application)} setRef={(node) => {
                                if (node) browseCardRefs.current.set(application.id, node);
                                else browseCardRefs.current.delete(application.id);
                              }} setupStatus={applicationSetupStatus(application, setupStatusByInterface)} />
                            )}
                            renderItemPreview={(application) => <ApplicationMiniLogo application={application} />}
                            resetKey={`${category}:${browseQuery}`}
                            showCount={Boolean(browseQuery.trim())}
                            title={CATEGORY_COPY[category].title}
                          />
                        ))}
                      </>
                    )}
                  </div>
                </div>
              </div>
            </section>}
          </CatalogRouteTransition>
        </div>
        <div aria-hidden={!detailOpen || undefined} className={cx("h-full min-h-0", !detailOpen && "pointer-events-none invisible absolute inset-0")}>
          <CatalogRouteTransition className="h-full min-h-0" direction={routeDirection} viewKey={selectedPack ? `application-pack-detail:${selectedPack.id}` : selected ? `application-detail:${selected.id}` : "application-detail-empty"}>
            {selectedPack ? (
              <ApplicationPackDetail
                accessProfile={accessProfile}
                extensionById={extensionById}
                clientSetup={clientSetup}
                clientSetupError={clientSetupError}
                lifecycle={packLifecycleOperations[selectedPack.id]}
                onApply={() => void applyApplicationPack(selectedPack)}
                onBack={closeApplicationPack}
                onRecommendedChange={(packageId, checked) => setPackRecommendedSelections((current) => ({
                  ...current,
                  [selectedPack.id]: togglePackRecommendation(selectedPack, current[selectedPack.id], packageId, checked),
                }))}
                pack={selectedPack}
                selectedRecommended={selectedPackRecommendations(selectedPack, packRecommendedSelections[selectedPack.id])}
                setupStatus={applicationPackSetupStatus(selectedPack, setupStatusByInterface)}
              />
            ) : selected ? (
              <ApplicationDetail accessProfile={accessProfile} application={selected} lifecycle={lifecycleOperations[selected.id]} onBack={closeApplication} onInstall={() => void installOrRepairApplication(selected)} onRemove={() => setRemoveId(selected.id)} onSetup={() => openSetup(selected)} projects={projects} rootEeId={rootEeId} setupStatus={applicationSetupStatus(selected, setupStatusByInterface)} />
            ) : <div className="h-full min-h-0" />}
          </CatalogRouteTransition>
        </div>
      </div>
      <ApplicationSetupDialog
        application={setupApplication}
        configurationDraft={configurationDraft}
        draft={setupDraft}
        onCancel={() => setSetupId("")}
        onChange={setSetupDraft}
        onConfigurationChange={setConfigurationDraft}
        onConfigurationSave={(interfaceId) => void saveConfiguration(interfaceId)}
        onSave={saveSetup}
        projects={projects}
        rootEeId={rootEeId}
        saving={saving}
      />
      <ApplicationRemovalDialog application={removeApplication} lifecycle={removeApplication ? lifecycleOperations[removeApplication.id] : undefined} onCancel={() => setRemoveId("")} onConfirm={() => removeApplication && void removeInstalledApplication(removeApplication)} />
      <SetupDialogShell ariaLabel="Review required packages" busy={false} open={Boolean(installReview)} onClose={() => setInstallReview(null)}>
        <h2 className="text-lg font-semibold">Install {installReview?.name}</h2>
        <p className="mt-2 text-sm text-text-secondary">This application requires these packages. Installing them does not connect them to tasks.</p>
        <ul className="my-4 space-y-2 text-sm">{installReview?.install_packages.map((entry) => <li key={entry.id}>{entry.name}{entry.installed ? " · Installed" : ""}</li>)}</ul>
        <div className="flex justify-end gap-2"><OcButton onClick={() => setInstallReview(null)}>Cancel</OcButton><OcButton onClick={() => { const application = installReview; setInstallReview(null); if (application) void installApplication(application); }}>Install</OcButton></div>
      </SetupDialogShell>
    </>
  );
}

function ApplicationPackCard({ accessProfile, clientSetup, clientSetupError, highlighted, lifecycle, onApply, onOpen, pack, setRef, setupStatus }: {
  accessProfile: OctopusClientAccessProfile;
  clientSetup: OctopusClientSetupState | null;
  clientSetupError: string;
  highlighted: boolean;
  lifecycle?: ApplicationPackLifecycleOperation;
  onApply: () => void;
  onOpen: () => void;
  pack: ApplicationPackRecord;
  setRef: (node: HTMLDivElement | null) => void;
  setupStatus?: ClientExtensionActionStatus;
}) {
  const inventory = applicationPackInventory(pack);
  const needsApply = applicationPackNeedsApply(pack);
  const busy = lifecycle && lifecycle.phase !== "failed" && lifecycle.phase !== "complete";
  const blockReason = applicationPackInstallBlockReason(pack);
  const actionLabel = lifecycle?.phase === "failed"
    ? "Retry"
    : inventory.state === "partial"
      ? "Finish install"
      : inventory.ready
        ? "Add recommended"
        : "Apply pack";
  return (
    <div
      className={cx(
        "group/card flex items-center justify-center gap-3 rounded-2xl border-none p-2 text-start text-base select-none outline-none hover:bg-text/5",
        highlighted && "bg-text/5",
      )}
      data-search-result-active={highlighted || undefined}
      ref={setRef}
      style={{ gridColumn: "1 / -1" }}
    >
      <button className="flex min-w-0 flex-1 cursor-interaction items-center gap-3 rounded-xl p-0 text-start outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0" data-application-pack-open onClick={onOpen} type="button">
        <ApplicationPackLogo />
        <span className="min-w-0 flex-1">
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate font-medium text-token-foreground">{pack.name}</span>
            <OcStatus label="Pack" tone="neutral" />
          </span>
          <span className="mt-0.5 block line-clamp-2 text-sm leading-relaxed text-text-description">{pack.description}</span>
          <ApplicationPackFactBadges clientSetup={clientSetup} clientSetupError={clientSetupError} compact pack={pack} setupStatus={setupStatus} />
        </span>
      </button>
      {needsApply ? (
        <div className="flex shrink-0 flex-col items-end gap-1">
          <OcButton
            disabled={Boolean(busy) || accessProfile !== "live" || Boolean(blockReason)}
            onClick={(event) => {
              event.stopPropagation();
              onApply();
            }}
            size="xs"
            title={accessProfile !== "live" ? "Apply packs from the Administrator profile." : blockReason || `Apply ${pack.name}`}
            variant="secondary"
          >
            {busy && lifecycle ? <ApplicationLifecycleProgress operation={lifecycle} /> : actionLabel}
          </OcButton>
          {lifecycle?.phase === "failed" ? <span className="max-w-48 truncate text-xs text-token-error-foreground">Install failed</span> : null}
        </div>
      ) : null}
    </div>
  );
}

function ApplicationPackDetail({ accessProfile, clientSetup, clientSetupError, extensionById, lifecycle, onApply, onBack, onRecommendedChange, pack, selectedRecommended, setupStatus }: {
  accessProfile: OctopusClientAccessProfile;
  clientSetup: OctopusClientSetupState | null;
  clientSetupError: string;
  extensionById: Map<string, ClientExtensionRecord>;
  lifecycle?: ApplicationPackLifecycleOperation;
  onApply: () => void;
  onBack: () => void;
  onRecommendedChange: (packageId: string, checked: boolean) => void;
  pack: ApplicationPackRecord;
  selectedRecommended: string[];
  setupStatus?: ClientExtensionActionStatus;
}) {
  const included = pack.components.filter((component) => component.selection === "included");
  const recommendedComponents = pack.components.filter((component) => component.selection === "recommended");
  const recommendedExtensions = pack.extensions.filter((extension) => extension.selection === "recommended");
  const inventory = applicationPackInventory(pack);
  const needsApply = applicationPackNeedsApply(pack, selectedRecommended);
  const busy = lifecycle && lifecycle.phase !== "failed" && lifecycle.phase !== "complete";
  const blockReason = applicationPackInstallBlockReason(pack, selectedRecommended);
  const actionLabel = lifecycle?.phase === "failed"
    ? "Retry"
    : inventory.state === "partial"
      ? "Finish install"
      : inventory.ready
        ? "Add recommended"
        : "Apply pack";
  const projectDefaults = pack.workspace_setup?.project_defaults.flatMap((entry) => {
    if (!entry.default_for_new_tasks) return [];
    const component = pack.components.find((candidate) => candidate.package_id === entry.package_id);
    return component ? [component.name] : [];
  }) ?? [];
  const homeProviderName = pack.components.find((component) => (
    component.product_id === pack.workspace_setup?.home.provider_interface_id
  ))?.name;
  return (
    <section className="flex h-full min-h-0 flex-col text-base select-none">
      <header className="surface-titlebar-spacer flex h-toolbar shrink-0 items-center px-panel">
        <button className="no-drag cursor-interaction flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-token-text-secondary hover:bg-token-list-hover-background hover:text-token-foreground focus:outline-none" onClick={onBack} type="button">
          <span>Applications</span>
          <span className="text-token-input-placeholder-foreground">/</span>
          <span className="truncate text-token-foreground">{pack.name}</span>
        </button>
      </header>
      <div className="plugins-surface-scroll min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]">
        <div className="flex min-h-full flex-col px-toolbar pt-panel pb-panel">
          <div className="@container mx-auto flex w-full max-w-[var(--thread-content-max-width)] flex-col gap-[var(--detail-page-section-gap)] [--detail-page-inline-inset:var(--padding-row-x)] [--detail-page-section-gap:calc(var(--spacing)*6)]">
            <header className="flex flex-col gap-4 px-[var(--detail-page-inline-inset,0px)]">
              <ApplicationPackLogo large />
              <div className="flex min-w-0 flex-wrap items-start justify-between gap-4">
                <div className="flex min-w-0 flex-1 basis-64 flex-col gap-1.5">
                  <div className="flex min-w-0 items-center gap-2">
                    <h1 className="heading-base min-w-0 break-words text-token-foreground electron:heading-lg">{pack.name}</h1>
                    <OcStatus label="Application Pack" tone="neutral" />
                  </div>
                  <div className="text-base text-token-text-secondary">{pack.description}</div>
                </div>
                <div className="flex max-w-full shrink-0 items-center gap-2">
                  <ApplicationPackFactBadges clientSetup={clientSetup} clientSetupError={clientSetupError} pack={pack} setupStatus={setupStatus} />
                  {needsApply ? (
                    <OcButton
                      disabled={Boolean(busy) || accessProfile !== "live" || Boolean(blockReason)}
                      onClick={onApply}
                      size="sm"
                      title={accessProfile !== "live" ? "Apply packs from the Administrator profile." : blockReason || `Apply ${pack.name}`}
                      variant="primary"
                    >
                      {busy && lifecycle ? <ApplicationLifecycleProgress operation={lifecycle} /> : actionLabel}
                    </OcButton>
                  ) : null}
                </div>
              </div>
            </header>
            {lifecycle?.phase === "failed" ? (
              <div className="mx-[var(--detail-page-inline-inset,0px)] rounded-xl border border-token-error-foreground/25 bg-token-error-background px-4 py-3 text-sm text-token-error-foreground" role="alert">
                <div className="font-medium">Pack installation failed</div>
                <div className="mt-1 text-xs opacity-90">Octopus could not finish applying this pack. Retry after resolving the incomplete item.</div>
              </div>
            ) : null}
            <DetailSection title="Setup">
              <div className="flex flex-col gap-2">
                <DetailRow label="Installed" value={`${inventory.installedCount} of ${inventory.includedCount} included components are fully installed and verified.`} />
                {applicationPackSetupVerified(pack, clientSetup) ? <DetailRow label="Workspace setup" value="Home, selected projects, local capabilities, and provider account access were verified together." /> : null}
                {setupStatus && inventory.ready ? <DetailRow label="Provider setup" value={setupStatus.message} /> : null}
                {homeProviderName ? <DetailRow label="Home" value={`During first setup, this pack connects Home through ${homeProviderName}.`} /> : null}
                <DetailRow label="Welcome defaults" value={projectDefaults.length ? `During first setup: ${projectDefaults.join(", ")}. Applying the pack here does not change your project choices.` : "This pack does not suggest any defaults during first setup."} />
                {recommendedExtensions.length ? <DetailRow label="Optional additions" value="Recommended client additions remain independent and can be installed from Extensions." /> : null}
              </div>
            </DetailSection>
            <DetailSection title="What’s in this pack">
              <div className="flex flex-col gap-2">
                <ApplicationPackDisclosure count={included.length} label="Included" open>
                  {included.map((component) => <ApplicationPackComponentRow component={component} key={component.package_id} />)}
                </ApplicationPackDisclosure>
                {recommendedComponents.length || recommendedExtensions.length ? (
                  <ApplicationPackDisclosure count={recommendedComponents.length + recommendedExtensions.length} label="Recommended">
                    {recommendedComponents.map((component) => (
                      <ApplicationPackComponentRow
                        component={component}
                        key={component.package_id}
                        selection={<OcSwitch checked={selectedRecommended.includes(component.package_id)} disabled={Boolean(busy) || !component.available} label={`Include ${component.name}`} onCheckedChange={(checked) => onRecommendedChange(component.package_id, checked)} />}
                      />
                    ))}
                    {recommendedExtensions.map((extension) => <ApplicationPackExtensionRow extension={extension} extensionRecord={extensionById.get(extension.extension_id)} key={extension.extension_id} />)}
                  </ApplicationPackDisclosure>
                ) : null}
              </div>
            </DetailSection>
            <DetailSection title="Permissions">
              <div className="flex flex-col gap-1">
                {included.map((component) => (
                  <DetailRow key={component.package_id} label={applicationPackPermissionLabel(component)} value={applicationPackPermissionDetail(component)} />
                ))}
                <DetailRow label="Project access" value="Capabilities are available only in Home and projects where you allow them; applying the pack does not turn them on for every task." />
              </div>
            </DetailSection>
            <DetailSection title="Information">
              <div className="flex flex-col gap-1">
                <DetailRow label="Version" value={pack.version} />
                <DetailRow label="Source" value="Reviewed local Application Pack" />
              </div>
            </DetailSection>
          </div>
        </div>
      </div>
    </section>
  );
}

function ApplicationPackDisclosure({ children, count, label, open = false }: { children: ReactNode; count: number; label: string; open?: boolean }) {
  return (
    <details className="group/section flex flex-col gap-4 [--sectioned-page-leading-inset:var(--detail-page-inline-inset,0px)]" open={open}>
      <summary className="flex cursor-interaction list-none items-center justify-between gap-3 border-b border-subtle pe-0.5 pb-2 text-default [padding-inline-start:var(--sectioned-page-leading-inset,0.5rem)] marker:hidden [&::-webkit-details-marker]:hidden">
        <span className="flex min-h-7 items-center gap-1.5 text-lg leading-6 font-medium">
          {label}
          <Glyph className="icon-2xs -rotate-90 transition-transform group-open/section:rotate-0 motion-reduce:transition-none" name="chevronDown" size={12} />
          <span className="text-tertiary">{count}</span>
        </span>
      </summary>
      <div className="flex flex-col gap-1 px-[var(--detail-page-inline-inset,0px)]">{children}</div>
    </details>
  );
}

function ApplicationPackComponentRow({ component, selection }: { component: ApplicationPackComponent; selection?: ReactNode }) {
  const status = applicationPackComponentStatus(component);
  return (
    <div className="flex min-h-14 items-center gap-3 rounded-xl px-2 py-2 hover:bg-token-foreground/3">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-token-foreground/5 text-token-text-secondary"><Glyph name={applicationPackComponentGlyph(component)} size={17} /></span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm font-medium text-token-foreground">{component.name}</span>
        <span className="block text-xs leading-4 text-token-description-foreground">{applicationPackComponentDescription(component)}</span>
      </span>
      {selection ?? <OcStatus label={status.label} tone={status.tone} />}
    </div>
  );
}

function ApplicationPackExtensionRow({ extension, extensionRecord }: { extension: ApplicationPackExtension; extensionRecord?: ClientExtensionRecord }) {
  const installed = extensionRecord?.installed === true;
  const configured = extensionRecord?.configured === true;
  return (
    <div className="flex min-h-14 items-center gap-3 rounded-xl px-2 py-2 hover:bg-token-foreground/3">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-token-foreground/5 text-token-text-secondary"><Glyph name={extensionRecord?.manifest.glyph ?? "plugins"} size={17} /></span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm font-medium text-token-foreground">{extensionRecord?.manifest.name ?? "Client extension"}</span>
        <span className="block text-xs leading-4 text-token-description-foreground">{extension.reason}</span>
      </span>
      <span className="flex shrink-0 flex-wrap justify-end gap-1">
        <OcStatus label={installed ? "Installed" : "Available in Extensions"} tone={installed ? "success" : "neutral"} />
        {installed && !configured ? <OcStatus label="Needs setup" tone="warning" /> : null}
      </span>
    </div>
  );
}

function ApplicationPackLogo({ large = false }: { large?: boolean }) {
  return (
    <span className={cx(
      "flex shrink-0 items-center justify-center overflow-hidden border border-default text-secondary",
      large ? "size-[60px] rounded-2xl bg-transparent" : "size-10 rounded-lg bg-transparent",
    )}>
      <Glyph name="layers" size={large ? 28 : 24} />
    </span>
  );
}

function ApplicationPackFactBadges({ clientSetup, clientSetupError, compact = false, pack, setupStatus }: { clientSetup: OctopusClientSetupState | null; clientSetupError: string; compact?: boolean; pack: ApplicationPackRecord; setupStatus?: ClientExtensionActionStatus }) {
  return (
    <span className={cx("flex min-w-0 flex-wrap items-center gap-1", compact && "mt-1")}>
      {applicationPackFacts(pack, setupStatus, clientSetup, clientSetupError).map((fact) => <OcStatus key={fact.label} label={fact.label} tone={fact.tone} />)}
    </span>
  );
}

function ManageApplicationsList({ accessProfile, applicationRefs, applications, highlightedId, lifecycleOperations, onInstall, onOpen, onRemove, onSetup, setupStatusByInterface }: {
  accessProfile: OctopusClientAccessProfile;
  applicationRefs: RefObject<Map<string, HTMLElement>>;
  applications: ApplicationRecord[];
  highlightedId: string;
  lifecycleOperations: Record<string, ApplicationLifecycleOperation>;
  onInstall: (application: ApplicationRecord) => void;
  onOpen: (id: string) => void;
  onRemove: (application: ApplicationRecord) => void;
  onSetup: (application: ApplicationRecord) => void;
  setupStatusByInterface: Map<string, ClientExtensionActionStatus>;
}) {
  return (
    <div className="grid grid-cols-1 gap-2">
      {applications.map((application) => (
        <ApplicationCard
          accessProfile={accessProfile}
          application={application}
          highlighted={highlightedId === application.id}
          key={application.id}
          lifecycle={lifecycleOperations[application.id]}
          onInstall={() => onInstall(application)}
          onOpen={onOpen}
          onRemove={() => onRemove(application)}
          onSetup={() => onSetup(application)}
          setupStatus={applicationSetupStatus(application, setupStatusByInterface)}
          view="manage"
          setRef={(node) => {
            if (node) applicationRefs.current?.set(application.id, node);
            else applicationRefs.current?.delete(application.id);
          }}
        />
      ))}
    </div>
  );
}

function ApplicationCard({
  accessProfile,
  application,
  highlighted,
  lifecycle,
  onInstall,
  onOpen,
  onRemove,
  onSetup,
  setupStatus,
  setRef,
  view = "browse",
}: {
  accessProfile: OctopusClientAccessProfile;
  application: ApplicationRecord;
  highlighted: boolean;
  lifecycle?: ApplicationLifecycleOperation;
  onInstall: () => void;
  onOpen: (id: string) => void;
  onRemove: () => void;
  onSetup?: () => void;
  setupStatus?: ClientExtensionActionStatus;
  setRef?: (node: HTMLDivElement | null) => void;
  view?: ApplicationCatalogView;
}) {
  return (
    <div
      className={cx(
        "group/card flex items-center justify-center gap-3 rounded-2xl border-none p-2 text-start text-base select-none outline-none hover:bg-text/5",
        highlighted && "bg-text/5",
      )}
      data-search-result-active={highlighted || undefined}
      ref={setRef}
    >
      <button className="flex min-w-0 flex-1 cursor-interaction items-center gap-3 rounded-xl p-0 text-start outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-0" data-application-open onClick={() => onOpen(application.id)} type="button">
        <ApplicationLogo application={application} className="size-10 overflow-hidden rounded-lg border border-default bg-transparent" logoSize={24} />
        <span className={cx("flex min-w-0 flex-1 flex-col justify-center", view === "browse" ? "gap-px" : "gap-0.5")}>
          <span className="block truncate font-medium text-default">{application.name}</span>
          <span className="block line-clamp-1 text-sm leading-relaxed text-text-description">{application.description}</span>
          <ApplicationCardMeta application={application} setupStatus={setupStatus} />
        </span>
      </button>
      {onSetup ? <ApplicationCardActions accessProfile={accessProfile} application={application} lifecycle={lifecycle} onInstall={onInstall} onRemove={onRemove} onSetup={onSetup} /> : null}
    </div>
  );
}

function ApplicationCardActions({ accessProfile, application, lifecycle, onInstall, onRemove, onSetup }: {
  accessProfile: OctopusClientAccessProfile;
  application: ApplicationRecord;
  lifecycle?: ApplicationLifecycleOperation;
  onInstall: () => void;
  onRemove: () => void;
  onSetup: () => void;
}) {
  const available = application.project_states.some((state) => state.policy_allowed);
  const busy = lifecycle && lifecycle.phase !== "failed" && lifecycle.phase !== "complete";
  const repairable = Boolean(applicationRepairExpectation(application));
  if (!application.installed) return (
    <div className="flex shrink-0 flex-col items-end gap-1">
      <OcButton
        disabled={Boolean(busy) || accessProfile !== "live" || !repairable && Boolean(application.install_block_reason)}
        onClick={(event) => {
          event.stopPropagation();
          onInstall();
        }}
        size="sm"
        title={accessProfile !== "live"
          ? `${repairable ? "Repair" : "Install"} applications from the Administrator profile.`
          : repairable ? "Repair application package" : application.install_block_reason || "Install application"}
        variant="outline"
      >
        {busy
          ? <ApplicationLifecycleProgress operation={lifecycle} />
          : lifecycle?.phase === "failed" ? `Retry ${repairable ? "repair" : "install"}` : repairable ? "Repair" : "Install"}
      </OcButton>
      {lifecycle?.phase === "failed" ? <span className="max-w-48 truncate text-xs text-token-error-foreground" title={lifecycle.error}>{lifecycle.operation === "repair" ? "Repair failed" : "Install failed"}</span> : null}
    </div>
  );
  return (
    <div className="flex shrink-0 flex-col items-end gap-1">
      <div className="flex items-center gap-1">
        {busy && lifecycle ? <ApplicationLifecycleProgress operation={lifecycle} /> : null}
        <OcButton
          className="shrink-0"
          disabled={!application.valid || Boolean(busy)}
          onClick={(event) => {
            event.stopPropagation();
            onSetup();
          }}
          size="xs"
          variant="ghost"
        >
          {!application.valid ? "View issue" : available ? "Manage" : "Set up"}
        </OcButton>
        {!application.managed ? <OcIconButton disabled={Boolean(busy) || accessProfile !== "live" || !application.removal_allowed} glyph="trash" label={`Remove ${application.name}`} onClick={(event) => {
          event.stopPropagation();
          onRemove();
        }} size="xs" title={accessProfile !== "live" ? "Remove applications from the Administrator profile." : application.removal_block_reason || `Remove ${application.name}`} variant="ghost" /> : null}
      </div>
      {lifecycle?.phase === "failed" && lifecycle.operation === "remove" ? <span className="max-w-48 truncate text-xs text-token-error-foreground" title={lifecycle.error}>Remove failed</span> : null}
    </div>
  );
}

function ApplicationDetail({
  accessProfile,
  application,
  lifecycle,
  onBack,
  onInstall,
  onRemove,
  onSetup,
  projects,
  rootEeId,
  setupStatus,
}: {
  accessProfile: OctopusClientAccessProfile;
  application: ApplicationRecord;
  lifecycle?: ApplicationLifecycleOperation;
  onBack: () => void;
  onInstall: () => void;
  onRemove: () => void;
  onSetup: () => void;
  projects: ApplicationProject[];
  rootEeId: string;
  setupStatus?: ClientExtensionActionStatus;
}) {
  const availableProjects = application.project_states.filter((state) => state.policy_allowed);
  const defaultProjects = application.project_states.filter((state) => state.selected_by_default);
  const partiallyConnectedRunCount = application.partially_connected_run_ids?.length ?? 0;
  const projectById = new Map(projects.map((project) => [project.ee_id, project]));
  const busy = lifecycle && lifecycle.phase !== "failed" && lifecycle.phase !== "complete";
  const repairable = Boolean(applicationRepairExpectation(application));
  const actionLabel = !application.installed
    ? lifecycle?.phase === "failed"
      ? `Retry ${repairable ? "repair" : "install"}`
      : repairable ? "Repair" : "Install"
    : !application.valid ? "View issue" : availableProjects.length ? "Manage" : "Set up";
  return (
    <section className="flex h-full min-h-0 flex-col text-base select-none">
      <header className="surface-titlebar-spacer flex h-toolbar shrink-0 items-center px-panel">
        <button className="no-drag cursor-interaction flex min-w-0 items-center gap-1.5 rounded-lg px-2 py-1 text-sm text-secondary hover:bg-primary-ghost-hover hover:text-default focus:outline-none" onClick={onBack} type="button">
          <span>Applications</span>
          <span className="text-tertiary">/</span>
          <span className="truncate text-default">{application.name}</span>
        </button>
      </header>
      <div className="plugins-surface-scroll min-h-0 flex-1 overflow-x-hidden overflow-y-auto [scrollbar-gutter:stable]">
        <div className="flex min-h-full flex-col px-toolbar pt-panel pb-panel">
          <div className="@container mx-auto flex w-full max-w-[var(--thread-content-max-width)] flex-col gap-[var(--detail-page-section-gap)] [--detail-page-inline-inset:var(--padding-row-x)] [--detail-page-section-gap:calc(var(--spacing)*6)]">
            <header className="flex flex-col gap-4 px-[var(--detail-page-inline-inset,0px)]">
              <ApplicationLogo application={application} className="size-[60px] overflow-hidden rounded-2xl border border-default bg-transparent" logoSize={28} />
              <div className="flex min-w-0 flex-wrap items-start justify-between gap-4">
                <div className="flex min-w-0 flex-1 basis-64 flex-col gap-1.5">
                  <h1 className="heading-base min-w-0 break-words text-default electron:heading-lg">{application.name}</h1>
                  <div className="text-base text-secondary">{application.description}</div>
                </div>
                <div className="flex max-w-full shrink-0 items-center gap-2">
                  <ApplicationFactBadges application={application} setupStatus={setupStatus} />
                  {application.installed ? (
                    <>
                      <OcButton disabled={Boolean(busy) || accessProfile !== "live" || !application.removal_allowed} onClick={onRemove} size="sm" title={accessProfile !== "live" ? "Remove applications from the Administrator profile." : application.removal_block_reason || `Remove ${application.name}`} tone="danger" variant="ghost">Remove</OcButton>
                      <OcButton disabled={Boolean(busy) || !application.valid} onClick={onSetup} size="sm" variant="primary">{busy && lifecycle ? <ApplicationLifecycleProgress operation={lifecycle} /> : actionLabel}</OcButton>
                    </>
                  ) : (
                    <OcButton
                      disabled={Boolean(busy) || accessProfile !== "live" || !repairable && Boolean(application.install_block_reason)}
                      leadingIcon={busy ? undefined : "download"}
                      onClick={onInstall}
                      size="sm"
                      title={accessProfile !== "live"
                        ? `${repairable ? "Repair" : "Install"} applications from the Administrator profile.`
                        : repairable ? "Repair application package" : application.install_block_reason || "Install application"}
                      variant="primary"
                    >
                      {busy && lifecycle ? <ApplicationLifecycleProgress operation={lifecycle} /> : actionLabel}
                    </OcButton>
                  )}
                </div>
              </div>
            </header>
            {lifecycle?.phase === "failed" ? (
              <div className="mx-[var(--detail-page-inline-inset,0px)] rounded-xl border border-token-error-foreground/25 bg-token-error-background px-4 py-3 text-sm text-token-error-foreground" role="alert">
                <div className="font-medium">{lifecycle.operation === "install" ? "Installation failed" : lifecycle.operation === "repair" ? "Repair failed" : "Removal failed"}</div>
                <div className="mt-1 whitespace-pre-wrap text-xs opacity-90">{lifecycle.error}</div>
              </div>
            ) : null}
            <DetailSection title="Setup">
              <div className="flex flex-col gap-2">
                <DetailRow label="Installed" value={application.installed
                  ? "Package installed in Octopus Home."
                  : application.materialized || application.partially_materialized
                    ? "Package files were found, but Octopus could not verify one complete installation."
                    : "Available to install. No interfaces or task bindings have been added."} />
                {application.configuration && application.configuration.fields.length ? <DetailRow label="Configuration" value={applicationConfigurationDetail(application.configuration)} /> : null}
                {setupStatus && application.installed ? <DetailRow label="Provider setup" value={setupStatus.message} /> : null}
                <DetailRow label="Enabled in" value={availableProjects.length ? availableProjects.map((state) => projectLabel(projectById.get(state.ee_id), state.ee_id, rootEeId)).join(", ") : "Not enabled yet. Choose Home or a project in Set up."} />
                <DetailRow label="Default for new tasks" value={defaultProjects.length ? defaultProjects.map((state) => projectLabel(projectById.get(state.ee_id), state.ee_id, rootEeId)).join(", ") : "Not selected by default in any project."} />
                <DetailRow label="Connected" value={partiallyConnectedRunCount
                  ? `Connection incomplete in ${partiallyConnectedRunCount} task${partiallyConnectedRunCount === 1 ? "" : "s"}.`
                  : application.connected_run_ids.length
                    ? `Connected to ${application.connected_run_ids.length} task${application.connected_run_ids.length === 1 ? "" : "s"}.`
                    : "Not connected to a task."} />
                {application.source_kind === "local" ? null : <DetailRow label="Package source" value="This build ships no package for it, so it cannot be repaired or updated." />}
                {application.installed && application.removal_block_reason ? <DetailRow label="Removal" value={application.removal_block_reason} /> : null}
                {!application.valid ? <DetailRow label="Installation issue" value={application.missing_interface_ids.length
                  ? `Missing interfaces: ${application.missing_interface_ids.join(", ")}`
                  : application.receipt_state && application.receipt_state !== "valid"
                    ? "The package files do not have one matching install receipt."
                    : "The package manifest could not be admitted."} /> : null}
              </div>
            </DetailSection>
            <DetailSection title="Included">
              <div className="flex flex-col gap-2">
                {application.interfaces.map((interfaceId) => <DetailRow key={interfaceId} label="Interface" value={interfaceId} />)}
                {application.artifact_bindings.map((binding) => <DetailRow key={binding.artifact_id} label="Guide for the model" value={binding.label?.trim() || binding.artifact_id} />)}
                <DetailRow label="Task access" value="A task gets it only when you connect it to that task." />
              </div>
            </DetailSection>
            <DetailSection title="Information">
              <div className="flex flex-col gap-1">
                <DetailRow label="Developer" value={application.developer} />
                <DetailRow label="Category" value={CATEGORY_COPY[application.category].title} />
                <DetailRow label="Version" value={application.version} />
                <DetailRow label="Manifest" value={application.manifest_path} />
              </div>
            </DetailSection>
          </div>
        </div>
      </div>
    </section>
  );
}

function ApplicationSetupDialog({
  application,
  configurationDraft,
  draft,
  onCancel,
  onChange,
  onConfigurationChange,
  onConfigurationSave,
  onSave,
  projects,
  rootEeId,
  saving,
}: {
  application: ApplicationRecord | null;
  configurationDraft: ConfigurationDraft;
  draft: SetupDraft;
  onCancel: () => void;
  onChange: (draft: SetupDraft) => void;
  onConfigurationChange: (draft: ConfigurationDraft) => void;
  onConfigurationSave: (interfaceId: string) => void;
  onSave: () => void;
  projects: ApplicationProject[];
  rootEeId: string;
  saving: boolean;
}) {
  const projectById = new Map(projects.map((project) => [project.ee_id, project]));
  const configuration = application ? applicationConfiguration(application) : applicationConfiguration(null);
  const availabilityDirty = application?.project_states.some((state) => {
    const current = draft[state.ee_id];
    return current && (current.available !== state.policy_allowed || current.selected !== state.selected_by_default);
  }) ?? false;
  return (
    <SetupDialogShell ariaLabel={application ? `Set up ${application.name}` : "Set up application"} busy={saving} onClose={onCancel} open={Boolean(application)} panelClassName="max-h-[min(720px,calc(100vh-48px))]" size="wide">
      {application ? (
        <>
        <div className="flex items-start gap-4 px-6 pt-8 pb-6">
          <ApplicationLogo application={application} className="size-[60px] overflow-hidden rounded-2xl border border-default bg-transparent" logoSize={28} />
          <div className="min-w-0 flex-1">
            <h2 className="heading-base text-token-foreground electron:heading-lg">Set up {application.name}</h2>
            <p className="mt-1 text-sm leading-5 text-token-text-secondary">Add the details this application needs, then choose where it is available. Existing tasks will not change.</p>
          </div>
          <OcIconButton disabled={saving} glyph="close" label="Close" onClick={onCancel} variant="ghost" />
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-6">
          {configuration.fields.length ? (
            <section className="mb-6">
              <div className="mb-2 flex items-center justify-between gap-3">
                <h3 className="text-sm font-medium text-token-foreground">Configuration</h3>
                <ConfigurationStatus status={configuration.status} />
              </div>
              <div className="space-y-4 rounded-2xl border border-token-border/60 p-4">
                {configuration.documents.map((document, documentIndex) => {
                  const fields = configuration.fields.filter((field) => field.interface_id === document.interface_id);
                  const dirty = fields.some((field) => {
                    const value = configurationDraft[configurationFieldKey(field)];
                    return Boolean(value?.clear || value?.value.length);
                  });
                  const conflictsResolved = fields.every((field) => (
                    field.source !== "conflict"
                    || (field.private_saved && configurationDraft[configurationFieldKey(field)]?.clear === true)
                  ));
                  const documentEditable = ["valid", "absent"].includes(document.status);
                  return (
                    <div className={cx(documentIndex > 0 && "border-t border-token-border/40 pt-4")} key={document.interface_id}>
                      <div className="space-y-3">
                      {fields.map((field) => {
                        const key = configurationFieldKey(field);
                        const current = configurationDraft[key] ?? { value: "", clear: false };
                        return <label className="block" key={key}>
                      <span className="mb-1.5 flex items-center justify-between gap-3 text-xs font-medium text-token-text-secondary">
                        <span>{field.label}{field.required ? " · Required" : ""}</span>
                        <span className={cx(field.source === "conflict" && "text-token-error-foreground")}>{configurationFieldStatus(field)}</span>
                      </span>
                      <div className="flex items-center gap-2">
                        <input
                          autoComplete="off"
                          className="h-9 min-w-0 flex-1 rounded-lg border border-token-border bg-token-input-background px-3 text-sm text-token-foreground outline-none placeholder:text-token-input-placeholder-foreground focus:border-token-focus-border disabled:opacity-50"
                          disabled={saving || current.clear || field.source === "conflict"}
                          onChange={(event) => onConfigurationChange({
                            ...configurationDraft,
                            [key]: { value: event.currentTarget.value, clear: false },
                          })}
                          placeholder={configurationFieldPlaceholder(field)}
                          type={field.kind === "secret" ? "password" : field.kind === "url" ? "url" : "text"}
                          value={current.value}
                        />
                        {field.private_saved ? (
                          <OcButton disabled={saving} onClick={() => onConfigurationChange({
                            ...configurationDraft,
                            [key]: { value: "", clear: !current.clear },
                          })} size="sm" tone={current.clear ? "danger" : "neutral"} variant="ghost">{current.clear ? "Keep" : "Clear"}</OcButton>
                        ) : null}
                      </div>
                      {current.clear ? <span className="mt-1 block text-xs text-token-error-foreground">Saved value will be cleared.</span> : null}
                        </label>;
                      })}
                      </div>
                      <div className="mt-3 flex justify-end">
                        <OcButton disabled={saving || !dirty || !documentEditable || !conflictsResolved} onClick={() => onConfigurationSave(document.interface_id)} size="sm" variant="secondary">Save connection details</OcButton>
                      </div>
                    </div>
                  );
                })}
              </div>
              {configuration.status === "issue" && configuration.error ? <p className="mt-2 text-xs text-token-error-foreground">{configuration.error}</p> : null}
            </section>
          ) : null}
          <h3 className="mb-2 text-sm font-medium text-token-foreground">Available in</h3>
          <div className="overflow-hidden rounded-2xl border border-token-border/60">
            {application.project_states.map((state, index) => {
              const current = draft[state.ee_id] ?? { available: state.policy_allowed, selected: state.selected_by_default };
              const project = projectById.get(state.ee_id);
              return (
                <div className={cx("flex items-center gap-4 px-4 py-3", index > 0 && "border-t border-token-border/40")} key={state.ee_id}>
                  <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-token-foreground/5 text-token-text-secondary"><Glyph name="project" size={17} /></span>
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm font-medium text-token-foreground">{projectLabel(project, state.ee_id, rootEeId)}</div>
                    <div className="truncate text-xs text-token-description-foreground">{state.ee_id === rootEeId ? "Root life area" : "Project"}</div>
                  </div>
                  <div className="flex shrink-0 items-center gap-4">
                    <OcSwitch checked={current.available} disabled={saving || (!state.ceiling_allowed && !current.available)} label={state.ceiling_allowed ? "Available" : current.available ? "Remove" : "Not supported here"} onCheckedChange={(available) => onChange({
                      ...draft,
                      [state.ee_id]: { available, selected: available ? current.selected : false },
                    })} showLabel />
                    <OcSwitch checked={current.selected} disabled={saving || !current.available} label="New task default" onCheckedChange={(selected) => onChange({
                      ...draft,
                      [state.ee_id]: { ...current, selected },
                    })} showLabel />
                  </div>
                </div>
              );
            })}
          </div>
        </div>
        <footer className="flex min-h-[76px] shrink-0 items-center justify-end gap-2 border-t border-token-border/40 px-6 py-4 [padding-bottom:max(1rem,env(safe-area-inset-bottom))]">
          <OcButton disabled={saving} onClick={onCancel} variant="ghost">Cancel</OcButton>
          <OcButton disabled={saving || !availabilityDirty} onClick={onSave} variant="primary">{saving ? "Saving…" : "Save availability"}</OcButton>
        </footer>
        </>
      ) : null}
    </SetupDialogShell>
  );
}

function ApplicationRemovalDialog({ application, lifecycle, onCancel, onConfirm }: {
  application: ApplicationRecord | null;
  lifecycle?: ApplicationLifecycleOperation;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const busy = lifecycle && lifecycle.phase !== "failed" && lifecycle.phase !== "complete";
  return (
    <SetupDialogShell ariaLabel={application ? `Remove ${application.name}` : "Remove application"} busy={Boolean(busy)} onClose={onCancel} open={Boolean(application)} panelClassName="max-w-md">
      {application ? (
        <>
          <div className="px-6 pt-8 pb-6">
            <h2 className="heading-base text-token-foreground electron:heading-lg">Remove {application.name}?</h2>
            <p className="mt-2 text-sm leading-5 text-token-text-secondary">Its interface folders will leave the active registry. Project policy and task bindings must already be removed; private interface data is not deleted.</p>
            {application.removal_block_reason ? <div className="mt-4 rounded-xl border border-token-charts-orange/25 bg-token-charts-orange/10 px-3 py-2 text-sm text-token-charts-orange">{application.removal_block_reason}</div> : null}
          </div>
          <footer className="flex min-h-[76px] shrink-0 items-center justify-end gap-2 border-t border-token-border/40 px-6 py-4">
            <OcButton disabled={Boolean(busy)} onClick={onCancel} variant="ghost">Cancel</OcButton>
            <OcButton disabled={Boolean(busy) || !application.removal_allowed} onClick={onConfirm} tone="danger" variant="secondary">{busy && lifecycle ? <ApplicationLifecycleProgress operation={lifecycle} /> : "Remove"}</OcButton>
          </footer>
        </>
      ) : null}
    </SetupDialogShell>
  );
}

function ApplicationLifecycleProgress({ operation }: { operation: ApplicationLifecycleOperation }) {
  const progress = Math.max(0, Math.min(100, operation.percent));
  const label = operation.operation === "repair"
    ? operation.phase === "preflight" || operation.phase === "verifying"
      ? "Checking"
      : operation.phase === "copying"
        ? "Copying"
        : operation.phase === "finalizing" || operation.phase === "receipting" || operation.phase === "complete"
          ? "Finishing"
          : "Repairing"
    : operation.operation === "install"
    ? operation.phase === "preflight" || operation.phase === "verifying"
      ? "Checking"
      : operation.phase === "copying"
        ? "Copying"
        : operation.phase === "finalizing" || operation.phase === "complete"
          ? "Finishing"
          : "Installing"
    : operation.phase === "preflight" || operation.phase === "verifying"
      ? "Checking"
      : operation.phase === "finalizing" || operation.phase === "complete"
        ? "Finishing"
        : "Removing";
  return <OcInlineProgress label={label} progressLabel={`${label} ${progress}%`} showValue value={progress} />;
}

function applicationRepairExpectation(application: ApplicationRecord): {
  sourceDigest: string;
  installedDigest: string;
  receiptState: "missing" | "stale";
  receiptDigest: string | null;
} | null {
  if (
    application.installed
    || !application.source_available
    || !application.materialized
    || application.partially_materialized
    || !application.package_id
    || !SHA256_DIGEST.test(application.source_digest)
    || !SHA256_DIGEST.test(application.installed_digest)
    || !["missing", "stale"].includes(application.receipt_state)
  ) return null;
  if (application.receipt_state === "missing") {
    if (application.receipt_digest) return null;
    return {
      sourceDigest: application.source_digest,
      installedDigest: application.installed_digest,
      receiptState: "missing",
      receiptDigest: null,
    };
  }
  if (!SHA256_DIGEST.test(application.receipt_digest)) return null;
  return {
    sourceDigest: application.source_digest,
    installedDigest: application.installed_digest,
    receiptState: "stale",
    receiptDigest: application.receipt_digest,
  };
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

function DetailRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="grid min-h-9 grid-cols-[minmax(7rem,0.35fr)_minmax(0,1fr)] items-start gap-4 rounded-lg px-2 py-1.5 hover:bg-token-foreground/3">
      <span className="text-sm text-token-text-secondary">{label}</span>
      <span className="min-w-0 text-sm text-default select-text [overflow-wrap:anywhere]">{value}</span>
    </div>
  );
}

function ApplicationFactBadges({ application, setupStatus }: { application: ApplicationRecord; setupStatus?: ClientExtensionActionStatus }) {
  const facts = applicationFacts(application, setupStatus);
  return (
    <span className="flex min-w-0 flex-wrap items-center gap-1">
      {facts.map((fact) => <OcStatus key={fact.label} label={fact.label} tone={fact.tone} />)}
    </span>
  );
}

function ApplicationCardMeta({ application, setupStatus }: { application: ApplicationRecord; setupStatus?: ClientExtensionActionStatus }) {
  const status = applicationCardStatus(application, setupStatus);
  const availableHome = application.project_states.some((state) => state.is_root && state.policy_allowed);
  const availableProjects = application.project_states.filter((state) => !state.is_root && state.policy_allowed).length;
  const scope = [
    availableHome ? "Home" : "",
    availableProjects ? `${availableProjects} project${availableProjects === 1 ? "" : "s"}` : "",
  ].filter(Boolean).join(" + ");
  return (
    <span className="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs leading-4 text-token-description-foreground">
      <i aria-hidden="true" className={cx(
        "size-1.5 shrink-0 rounded-full",
        status.tone === "success" ? "bg-token-charts-green"
          : status.tone === "warning" ? "bg-token-charts-orange"
            : status.tone === "danger" ? "bg-token-charts-red"
              : "bg-token-description-foreground",
      )} />
      <span className={cx(
        "shrink-0",
        status.tone === "warning" && "text-token-charts-orange",
        status.tone === "danger" && "text-token-charts-red",
      )}>{status.label}</span>
      {scope ? <><span aria-hidden="true">·</span><span className="truncate">{scope}</span></> : null}
    </span>
  );
}

function applicationCardStatus(application: ApplicationRecord, setupStatus?: ClientExtensionActionStatus): { label: string; tone: "neutral" | "success" | "warning" | "danger" } {
  const materializationIncomplete = !application.installed && (
    application.materialized
    || application.partially_materialized
    || application.receipt_state === "stale"
  );
  if (materializationIncomplete || (application.installed && !application.valid)) return { label: "Needs repair", tone: "danger" };
  if (application.configuration?.status === "issue") return { label: "Check settings", tone: "danger" };
  if (application.partially_connected_run_ids?.length) return { label: "Connection incomplete", tone: "danger" };
  if (setupStatus?.state === "unavailable") return { label: "Setup unavailable", tone: "danger" };
  if (setupStatus?.state === "repair_available" || setupStatus?.state === "sign_in_required") return { label: "Sign in required", tone: "warning" };
  if (application.configuration?.status === "required") return { label: "Needs setup", tone: "warning" };
  if (setupStatus?.state === "waiting_for_interface") return { label: "Finishing setup", tone: "neutral" };
  if (application.installed && !application.project_states.some((state) => state.policy_allowed)) return { label: "Not enabled", tone: "neutral" };
  if (application.installed) return { label: "Enabled", tone: "success" };
  return application.source_available
    ? { label: "Not installed", tone: "neutral" }
    : { label: "Unavailable", tone: "danger" };
}

function applicationFacts(application: ApplicationRecord, setupStatus?: ClientExtensionActionStatus): Array<{ label: string; tone: "neutral" | "success" | "warning" | "danger" }> {
  const facts = [applicationCardStatus(application, setupStatus)];
  const connected = application.connected_run_ids.length;
  if (connected) facts.push({ label: `Connected to ${connected} task${connected === 1 ? "" : "s"}`, tone: "success" });
  return facts;
}

function applicationConfiguration(application: ApplicationRecord | null): ApplicationConfiguration {
  return application?.configuration ?? { status: "not_installed", fields: [], documents: [], error: "" };
}

function applicationConfigurationDetail(configuration: ApplicationConfiguration): string {
  if (configuration.status === "issue") return configuration.error || "Configuration could not be verified.";
  if (configuration.status === "required") return "Setup required before the application can use its external service.";
  if (configuration.status === "unknown") return "Octopus cannot see the runtime environment here, so it cannot tell whether these details are set.";
  if (configuration.fields.some((field) => field.private_saved)) return "Saved in Octopus. A task shows whether the service answers.";
  if (configuration.fields.some((field) => ["environment", "both"].includes(field.source))) return "Set in the environment Octopus starts with.";
  return "Nothing to configure.";
}

function configurationFieldKey(field: ApplicationConfigurationField): string {
  return `${field.interface_id}\0${field.name}`;
}

function configurationFieldStatus(field: ApplicationConfigurationField): string {
  if (field.source === "conflict") return "Configuration issue";
  if (field.private_saved) return "Saved";
  if (field.source === "environment") return "Set in environment";
  if (field.source === "unknown") return "Not checked";
  return field.required ? "Not configured" : "Optional";
}

function configurationFieldPlaceholder(field: ApplicationConfigurationField): string {
  if (field.private_saved) return "Leave blank to keep saved value";
  if (field.source === "environment") return "Configured outside Octopus";
  if (field.source === "conflict") return "Resolve the conflicting external value first";
  return field.required ? `Enter ${field.label.toLowerCase()}` : `Optional ${field.label.toLowerCase()}`;
}

function ConfigurationStatus({ status }: { status: ApplicationConfiguration["status"] }) {
  if (status === "issue") return <OcStatus label="Configuration issue" tone="danger" />;
  if (status === "required") return <OcStatus label="Setup required" tone="warning" />;
  if (status === "unknown") return <OcStatus label="Not checked" tone="neutral" />;
  return <OcStatus label="Configured" tone="neutral" />;
}

function applicationSetupStatus(
  application: ApplicationRecord,
  byInterface: Map<string, ClientExtensionActionStatus>,
): ClientExtensionActionStatus | undefined {
  for (const interfaceId of application.interfaces) {
    const status = byInterface.get(interfaceId);
    if (status) return status;
  }
  return byInterface.get(application.id);
}

function ApplicationLogo({ application, className, logoSize }: { application: ApplicationRecord; className?: string; logoSize: number }) {
  const lightArtwork = application.logo_data_url?.trim();
  const darkArtwork = application.logo_dark_data_url?.trim();
  const artwork = lightArtwork || darkArtwork;
  const hasThemeArtwork = Boolean(lightArtwork && darkArtwork && lightArtwork !== darkArtwork);
  return (
    <span className={cx(
      "flex shrink-0 items-center justify-center text-secondary",
      className,
    )}>
      {artwork ? (
        <>
          <img alt="" className={cx("size-full rounded-2xs object-contain", hasThemeArtwork && "application-artwork-light")} draggable={false} src={artwork} />
          {hasThemeArtwork ? <img alt="" className="application-artwork-dark size-full rounded-2xs object-contain" draggable={false} src={darkArtwork} /> : null}
        </>
      ) : <Glyph name={applicationGlyph(application)} size={logoSize} />}
    </span>
  );
}

function ApplicationMiniLogo({ application }: { application: ApplicationRecord }) {
  return <ApplicationLogo application={application} className="size-[13px] overflow-hidden rounded-[4px] bg-surface ring-1 ring-border ring-inset" logoSize={8} />;
}

function SurfaceState({ title, detail = "", compact = false, actionLabel = "", onAction }: { title: string; detail?: string; compact?: boolean; actionLabel?: string; onAction?: () => void }) {
  return (
    <OcEmptyState
      action={actionLabel && onAction ? <OcButton onClick={onAction} size="xs" variant="secondary">{actionLabel}</OcButton> : undefined}
      className={compact ? "flex-1" : "h-full min-h-0"}
      detail={detail || undefined}
      size={compact ? "compact" : "default"}
      title={title}
    />
  );
}

function ApplicationProjectionIssue({ detail }: { detail: string }) {
  return (
    <div className="rounded-xl border border-token-error-foreground/25 bg-token-error-background px-4 py-3 text-sm text-token-error-foreground" role="alert">
      <div className="font-medium">Some application packages need attention</div>
      <div className="mt-1 whitespace-pre-wrap text-xs opacity-90">{detail}</div>
    </div>
  );
}

function applicationGlyph(application: ApplicationRecord): GlyphName {
  const known = new Set<GlyphName>(["plugins", "image", "voice", "file", "artifact", "interfaces", "binding", "chat", "send", "scheduled", "folder"]);
  const candidate = application.icon as GlyphName;
  if (known.has(candidate)) return candidate;
  if (application.icon === "message") return "chat";
  if (application.icon === "email") return "send";
  if (application.icon === "files") return "folder";
  if (application.icon === "calendar") return "scheduled";
  if (application.category === "plugin") return "plugins";
  if (application.category === "artifact") return "artifact";
  return "interfaces";
}

function applicationSearchText(application: ApplicationRecord): string {
  return [
    application.id,
    application.name,
    application.description,
    application.category,
    application.developer,
    ...application.interfaces,
  ].join(" ").toLowerCase();
}

function projectLabel(project: ApplicationProject | undefined, eeId: string, rootEeId: string): string {
  if (eeId === rootEeId) return "Home";
  return project?.name || "Untitled project";
}

function applicationPackKey(packId: string): string {
  return `pack:${packId}`;
}

function applicationPackSearchText(pack: ApplicationPackRecord): string {
  return [
    pack.id,
    pack.name,
    pack.description,
    ...pack.components.flatMap((component) => [component.name, component.reason]),
  ].join(" ").toLowerCase();
}

function applicationPackInventory(pack: ApplicationPackRecord): {
  ready: boolean;
  state: "available" | "partial" | "installed";
  includedCount: number;
  installedCount: number;
} {
  const included = pack.components.filter((component) => component.selection === "included");
  const installedCount = included.filter((component) => component.installed && component.receipt_state === "valid").length;
  const ready = included.length > 0
    && pack.required_components_materialized
    && installedCount === included.length;
  const partial = !ready && (
    installedCount > 0
    || included.some((component) => (
      component.materialized
      || component.partially_materialized
      || component.receipt_state === "stale"
      || Boolean(component.error)
    ))
  );
  return {
    ready,
    state: ready ? "installed" : partial ? "partial" : "available",
    includedCount: included.length,
    installedCount,
  };
}

function selectedPackRecommendations(pack: ApplicationPackRecord, selected: string[] | undefined): string[] {
  const recommended = pack.components.filter((component) => component.selection === "recommended");
  const allowed = new Set(recommended.map((component) => component.package_id));
  const source = selected ?? recommended.filter((component) => component.available).map((component) => component.package_id);
  return [...new Set(source.filter((packageId) => allowed.has(packageId)))].sort();
}

function togglePackRecommendation(
  pack: ApplicationPackRecord,
  current: string[] | undefined,
  packageId: string,
  checked: boolean,
): string[] {
  const selected = new Set(selectedPackRecommendations(pack, current));
  if (checked) selected.add(packageId);
  else selected.delete(packageId);
  return selectedPackRecommendations(pack, [...selected]);
}

function applicationPackInstallBlockReason(pack: ApplicationPackRecord, selectedRecommended?: string[]): string {
  const selected = new Set(selectedRecommended ?? selectedPackRecommendations(pack, undefined));
  const components = pack.components.filter((component) => (
    component.selection === "included" || selected.has(component.package_id)
  ));
  if (!components.length) return "This pack contains no installable components.";
  if (components.some((component) => !component.available || !component.source_digest)) {
    return "One or more selected components are unavailable in the local library.";
  }
  if (components.some((component) => (
    !component.installed
    && (
      component.materialized
      || component.partially_materialized
      || component.receipt_state === "stale"
      || Boolean(component.error)
    )
  ))) {
    return "An installed component needs attention before this pack can be applied.";
  }
  return "";
}

function applicationPackNeedsApply(pack: ApplicationPackRecord, selectedRecommended?: string[]): boolean {
  const selected = new Set(selectedRecommended ?? selectedPackRecommendations(pack, undefined));
  return pack.components.some((component) => (
    (component.selection === "included" || selected.has(component.package_id))
    && (!component.installed || component.receipt_state !== "valid")
  ));
}

function applicationPackSetupStatus(
  pack: ApplicationPackRecord,
  byInterface: Map<string, ClientExtensionActionStatus>,
): ClientExtensionActionStatus | undefined {
  const providerInterfaceId = pack.workspace_setup?.home.provider_interface_id;
  return providerInterfaceId ? byInterface.get(providerInterfaceId) : undefined;
}

function applicationPackFacts(
  pack: ApplicationPackRecord,
  setupStatus?: ClientExtensionActionStatus,
  clientSetup?: OctopusClientSetupState | null,
  clientSetupError = "",
): Array<{ label: string; tone: "neutral" | "success" | "warning" | "danger" }> {
  const inventory = applicationPackInventory(pack);
  const setupVerified = applicationPackSetupVerified(pack, clientSetup);
  const currentSetupFailure = setupStatus && setupStatus.state !== "configured";
  const facts: Array<{ label: string; tone: "neutral" | "success" | "warning" | "danger" }> = [
    inventory.state === "installed"
      ? { label: "Installed", tone: "success" }
      : inventory.state === "partial"
        ? { label: "Partially installed", tone: "warning" }
        : { label: "Not installed", tone: "neutral" },
  ];
  if (inventory.ready && clientSetupError) facts.push({ label: "Setup unknown", tone: "danger" });
  else if (inventory.ready && setupStatus?.state === "unavailable") facts.push({ label: "Setup unavailable", tone: "danger" });
  else if (inventory.ready && (currentSetupFailure || !setupVerified)) facts.push({ label: "Needs setup", tone: "warning" });
  else if (inventory.ready) facts.push({ label: "Ready", tone: "success" });
  return facts;
}

function applicationPackSetupVerified(
  pack: ApplicationPackRecord,
  setup: OctopusClientSetupState | null | undefined,
): boolean {
  return setup?.status === "complete"
    && Boolean(setup.receiptDigest)
    && setup.plan.pack.id === pack.id
    && setup.plan.pack.manifest_digest === pack.manifest_digest;
}

function applicationPackComponentStatus(component: ApplicationPackComponent): { label: string; tone: "neutral" | "success" | "warning" | "danger" } {
  if (component.installed && component.receipt_state === "valid") return { label: "Installed", tone: "success" };
  if (
    component.installed
    || component.materialized
    || component.partially_materialized
    || component.receipt_state === "stale"
    || component.error
  ) return { label: "Needs repair", tone: "danger" };
  if (component.available) return { label: component.selection === "included" ? "Included" : "Optional", tone: "neutral" };
  return { label: "Unavailable", tone: "danger" };
}

function applicationPackComponentGlyph(component: ApplicationPackComponent): GlyphName {
  if (component.product_kind === "driver") return "environment";
  if (component.product_kind === "artifact") return "book";
  return "interfaces";
}

function applicationPackComponentDescription(component: ApplicationPackComponent): string {
  if (component.product_kind === "driver") return "Local capability available only where you allow it.";
  if (component.product_kind === "artifact") return "Reference context for tasks that include it.";
  return "Communication capability for Home and selected projects.";
}

function applicationPackPermissionLabel(component: ApplicationPackComponent): string {
  if (component.product_kind === "driver") return `Use ${component.name}`;
  if (component.product_kind === "artifact") return `Read ${component.name}`;
  return `Connect through ${component.name}`;
}

function applicationPackPermissionDetail(component: ApplicationPackComponent): string {
  if (component.product_kind === "driver") return "Allows local operations only inside projects where this capability is enabled.";
  if (component.product_kind === "artifact") return "Shares this reference context only with tasks that include it.";
  return "Allows task messages and results to use this connection only in projects where you turn it on.";
}

function arrayRecords<T>(value: T[] | undefined): T[] {
  return Array.isArray(value) ? value : [];
}

function arrayStrings(value: unknown): string[] {
  return Array.isArray(value) ? value.map((item) => typeof item === "string" ? item : "").filter(Boolean) : [];
}
