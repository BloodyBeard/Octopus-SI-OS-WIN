import type {
  OctopusAdministratorAccessUpdate,
  OctopusClientAccessDetails,
  OctopusClientAccessStatus,
  OctopusClientLabAccess,
  OctopusTouchIdStatus,
} from "./electron";

const ACCESS_URL = "/__octopus/access";

export async function readClientAccessStatus(): Promise<OctopusClientAccessStatus> {
  if (window.octopusTransport?.accessStatus) return window.octopusTransport.accessStatus();
  return request("status", "GET");
}

export async function configureClientAccess(password: string): Promise<OctopusClientAccessStatus> {
  if (window.octopusTransport?.configureAccess) return window.octopusTransport.configureAccess(password);
  return request("configure", "POST", { password });
}

export async function unlockClientAccess(password: string): Promise<OctopusClientAccessStatus> {
  if (window.octopusTransport?.unlockAccess) return window.octopusTransport.unlockAccess(password);
  return request("unlock", "POST", { password });
}

export async function readTouchIdStatus(): Promise<OctopusTouchIdStatus> {
  if (window.octopusTransport?.touchIdStatus) return window.octopusTransport.touchIdStatus();
  return { available: false, enrolled: false, prompted: false };
}

export async function setTouchIdEnabled(enabled: boolean): Promise<OctopusTouchIdStatus> {
  if (window.octopusTransport?.setTouchIdEnabled) return window.octopusTransport.setTouchIdEnabled(enabled);
  throw new Error("Touch ID is unavailable outside the Octopus desktop app.");
}

export async function unlockWithTouchId(): Promise<OctopusClientAccessStatus> {
  if (window.octopusTransport?.unlockWithTouchId) return window.octopusTransport.unlockWithTouchId();
  throw new Error("Touch ID is unavailable outside the Octopus desktop app.");
}

export async function lockClientAccess(): Promise<OctopusClientAccessStatus> {
  if (window.octopusTransport?.lockAccess) return window.octopusTransport.lockAccess();
  return request("lock", "POST", {});
}

export async function readClientLabAccess(): Promise<OctopusClientLabAccess> {
  if (window.octopusTransport?.labAccess) return window.octopusTransport.labAccess();
  return labRequest("lab-status", "GET");
}

export async function updateClientLabAccess(password: string): Promise<OctopusClientLabAccess> {
  if (window.octopusTransport?.updateLabAccess) return window.octopusTransport.updateLabAccess(password);
  return labRequest("lab-update", "POST", { password });
}

export async function readClientAccessDetails(): Promise<OctopusClientAccessDetails> {
  if (window.octopusTransport?.accessDetails) return window.octopusTransport.accessDetails();
  return detailsRequest("details", "GET");
}

export async function updateClientAdministratorAccess(
  request: OctopusAdministratorAccessUpdate,
): Promise<OctopusClientAccessDetails> {
  if (window.octopusTransport?.updateAdministratorAccess) {
    return window.octopusTransport.updateAdministratorAccess(request);
  }
  return detailsRequest("administrator-update", "POST", request);
}

async function request(
  operation: "status" | "configure" | "unlock" | "lock",
  method: "GET" | "POST",
  body?: Record<string, unknown>,
): Promise<OctopusClientAccessStatus> {
  if (!import.meta.env.DEV) throw new Error("Client access host is unavailable");
  const response = await fetch(`${ACCESS_URL}/${operation}`, {
    method,
    credentials: "same-origin",
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
    cache: "no-store",
  });
  const payload = await response.json();
  if (!response.ok) throw new Error(typeof payload?.error === "string" ? payload.error : `Client access failed: HTTP ${response.status}`);
  return payload as OctopusClientAccessStatus;
}

async function labRequest(
  operation: "lab-status" | "lab-update",
  method: "GET" | "POST",
  body?: Record<string, unknown>,
): Promise<OctopusClientLabAccess> {
  if (!import.meta.env.DEV) throw new Error("Client access host is unavailable");
  const response = await fetch(`${ACCESS_URL}/${operation}`, {
    method,
    credentials: "same-origin",
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
    cache: "no-store",
  });
  const payload = await response.json();
  if (!response.ok) throw new Error(typeof payload?.error === "string" ? payload.error : `Client access failed: HTTP ${response.status}`);
  return payload as OctopusClientLabAccess;
}

async function detailsRequest(
  operation: "details" | "administrator-update",
  method: "GET" | "POST",
  body?: OctopusAdministratorAccessUpdate,
): Promise<OctopusClientAccessDetails> {
  if (!import.meta.env.DEV) throw new Error("Client access host is unavailable");
  const response = await fetch(`${ACCESS_URL}/${operation}`, {
    method,
    credentials: "same-origin",
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
    cache: "no-store",
  });
  const payload = await response.json();
  if (!response.ok) throw new Error(typeof payload?.error === "string" ? payload.error : `Client access failed: HTTP ${response.status}`);
  return payload as OctopusClientAccessDetails;
}
