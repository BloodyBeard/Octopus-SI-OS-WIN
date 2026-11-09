export const ONBOARDING_STORAGE_KEY = "octopus.onboarding.v1";
const WELCOME_PROGRESS_KEY = "octopus.welcome-progress.v1";

export type WelcomeProgress = { step: number; heroSeen: boolean };

export function readWelcomeProgress(): WelcomeProgress {
  const raw = window.localStorage.getItem(WELCOME_PROGRESS_KEY);
  if (raw === null) return { step: 0, heroSeen: false };
  const value = JSON.parse(raw) as WelcomeProgress;
  if (!value || !Number.isInteger(value.step) || value.step < 0 || value.step > 3 || typeof value.heroSeen !== "boolean") {
    throw new Error("Saved welcome progress is invalid.");
  }
  return value;
}

export function saveWelcomeProgress(progress: WelcomeProgress) {
  window.localStorage.setItem(WELCOME_PROGRESS_KEY, JSON.stringify(progress));
}

export type OnboardingState = {
  version: 1;
  completed: boolean;
  visitCount: number;
  firstSeenAt: string;
  lastSeenAt: string;
  completedAt?: string;
};

type StoredOnboardingState = Partial<OnboardingState>;

function nowIso() {
  return new Date().toISOString();
}

function initialState(timestamp = nowIso()): OnboardingState {
  return {
    version: 1,
    completed: false,
    visitCount: 0,
    firstSeenAt: timestamp,
    lastSeenAt: timestamp,
  };
}

function readStoredState(): OnboardingState {
  const timestamp = nowIso();
  try {
    const raw = window.localStorage.getItem(ONBOARDING_STORAGE_KEY);
    if (!raw) return initialState(timestamp);
    const parsed = JSON.parse(raw) as StoredOnboardingState;
    return {
      version: 1,
      completed: parsed.completed === true,
      visitCount: typeof parsed.visitCount === "number" && Number.isFinite(parsed.visitCount)
        ? Math.max(0, Math.floor(parsed.visitCount))
        : 0,
      firstSeenAt: typeof parsed.firstSeenAt === "string" ? parsed.firstSeenAt : timestamp,
      lastSeenAt: typeof parsed.lastSeenAt === "string" ? parsed.lastSeenAt : timestamp,
      ...(typeof parsed.completedAt === "string" ? { completedAt: parsed.completedAt } : {}),
    };
  } catch (error) {
    throw new Error(`Cannot read ${ONBOARDING_STORAGE_KEY}: ${error instanceof Error ? error.message : String(error)}`);
  }
}

function writeStoredState(state: OnboardingState) {
  try {
    window.localStorage.setItem(ONBOARDING_STORAGE_KEY, JSON.stringify(state));
  } catch (error) {
    throw new Error(`Cannot write ${ONBOARDING_STORAGE_KEY}: ${error instanceof Error ? error.message : String(error)}`);
  }
}

export function resetOnboardingState() {
  try {
    window.localStorage.removeItem(ONBOARDING_STORAGE_KEY);
    window.localStorage.removeItem(WELCOME_PROGRESS_KEY);
  } catch (error) {
    throw new Error(`Cannot reset ${ONBOARDING_STORAGE_KEY}: ${error instanceof Error ? error.message : String(error)}`);
  }
}

export function recordOnboardingVisit(): OnboardingState {
  const timestamp = nowIso();
  const current = readStoredState();
  const next: OnboardingState = {
    ...current,
    version: 1,
    visitCount: current.visitCount + 1,
    lastSeenAt: timestamp,
  };
  writeStoredState(next);
  return next;
}

export function completeOnboarding(): OnboardingState {
  const timestamp = nowIso();
  const current = readStoredState();
  const next: OnboardingState = {
    ...current,
    version: 1,
    completed: true,
    completedAt: timestamp,
    lastSeenAt: timestamp,
  };
  writeStoredState(next);
  return next;
}
