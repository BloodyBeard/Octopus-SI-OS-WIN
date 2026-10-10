import type { HandLandmarker, HandLandmarkerResult, NormalizedLandmark } from "@mediapipe/tasks-vision";
import { useSyncExternalStore } from "react";
import { AirHandGestureEngine, type AirHandPoint, type AirHandPose, type FingerCurls } from "./airHandEngine";
import { emitAirHandGesture, type GestureSensitivity } from "./gestureShortcuts";

export type GestureCamera = {
  deviceId: string;
  label: string;
};

export type GestureRecognitionPhase = "off" | "requesting" | "loading" | "running" | "denied" | "error";

export type GestureRecognitionState = Readonly<{
  activeDeviceId: string;
  confidence: number;
  devices: readonly GestureCamera[];
  error: string;
  phase: GestureRecognitionPhase;
  point: AirHandPoint | null;
  palmPoint: AirHandPoint | null;
  pinchRatio: number | null;
  curls: FingerCurls | null;
  pose: AirHandPose;
}>;

export type GestureDevicesView = Readonly<{
  devices: readonly GestureCamera[];
  phase: GestureRecognitionPhase;
  error: string;
  activeDeviceId: string;
}>;

export type GestureRecognitionOptions = {
  cameraDeviceId: string;
  mirrorX: boolean;
  sensitivity: GestureSensitivity;
};

const INITIAL_STATE: GestureRecognitionState = Object.freeze({
  activeDeviceId: "",
  confidence: 0,
  devices: [],
  error: "",
  phase: "off",
  point: null,
  palmPoint: null,
  pinchRatio: null,
  curls: null,
  pose: "none",
});

const IDLE_HAND = Object.freeze({ point: null, palmPoint: null, pinchRatio: null, curls: null, pose: "none", confidence: 0 }) satisfies Partial<GestureRecognitionState>;
const STALE_FRAME_MS = 1_200;

const HAND_CONNECTIONS: ReadonlyArray<readonly [number, number]> = [
  [0, 1], [1, 2], [2, 3], [3, 4],
  [0, 5], [5, 6], [6, 7], [7, 8],
  [5, 9], [9, 10], [10, 11], [11, 12],
  [9, 13], [13, 14], [14, 15], [15, 16],
  [13, 17], [17, 18], [18, 19], [19, 20],
  [0, 17],
];

class GestureRecognitionController {
  private state = INITIAL_STATE;
  private subscribers = new Set<() => void>();
  private stream: MediaStream | null = null;
  private video: HTMLVideoElement | null = null;
  private frame = 0;
  private generation = 0;
  private configKey = "";
  private landmarkerPromise: Promise<HandLandmarker> | null = null;
  private engine = new AirHandGestureEngine();
  private lastVideoTime = -1;
  private lastPublishedAt = 0;
  private preview: HTMLCanvasElement | null = null;
  private lastLandmarks: NormalizedLandmark[] | null = null;
  private lastOptions: GestureRecognitionOptions | null = null;
  private wanted: GestureRecognitionOptions | null = null;
  private lastFrameAt = 0;
  private watching = false;

  attachPreview = (canvas: HTMLCanvasElement | null): void => {
    this.preview = canvas;
    this.drawPreview();
  };

  private coarse: GestureRecognitionState = INITIAL_STATE;
  private coarseAt = 0;
  private devicesView: GestureDevicesView = {
    devices: INITIAL_STATE.devices,
    phase: INITIAL_STATE.phase,
    error: INITIAL_STATE.error,
    activeDeviceId: INITIAL_STATE.activeDeviceId,
  };

  subscribe = (subscriber: () => void): (() => void) => {
    this.subscribers.add(subscriber);
    return () => {
      this.subscribers.delete(subscriber);
    };
  };

  snapshot = (): GestureRecognitionState => this.state;

  coarseSnapshot = (): GestureRecognitionState => this.coarse;

  devicesSnapshot = (): GestureDevicesView => this.devicesView;

  async start(options: GestureRecognitionOptions): Promise<void> {
    const key = JSON.stringify(options);
    this.lastOptions = options;
    this.wanted = options;
    this.watchEnvironment();
    if (this.configKey === key && (this.state.phase === "requesting" || this.state.phase === "loading" || this.state.phase === "running")) return;
    this.stopCapture();
    this.configKey = key;
    const generation = ++this.generation;
    if (!navigator.mediaDevices?.getUserMedia) {
      this.publish({ phase: "error", error: "Camera capture is unavailable in this client." });
      return;
    }
    if (document.hidden) {
      this.configKey = "";
      this.publish({ ...IDLE_HAND, phase: "off", error: "" });
      return;
    }
    this.publish({ phase: "requesting", error: "", point: null, pose: "none", confidence: 0 });
    try {
      const nativePermission = await window.octopusTransport?.cameraAccessStatus?.();
      if (generation !== this.generation) return;
      if (nativePermission && !nativePermission.granted) {
        this.publish({ phase: "denied", error: `Camera permission is ${nativePermission.status}.` });
        return;
      }
      const stream = await openCamera(options.cameraDeviceId);
      if (generation !== this.generation) {
        stopStream(stream);
        return;
      }
      this.stream = stream;
      stream.getVideoTracks()[0]?.addEventListener("ended", () => {
        if (generation !== this.generation || this.stream !== stream) return;
        this.configKey = "";
        this.stopCapture();
        this.publish({ ...IDLE_HAND, phase: "off", error: "" });
        if (this.wanted) void this.start(this.wanted);
      }, { once: true });
      const video = document.createElement("video");
      video.autoplay = true;
      video.muted = true;
      video.playsInline = true;
      video.srcObject = stream;
      this.video = video;
      await video.play();
      if (generation !== this.generation) return;
      this.publish({ phase: "loading" });
      const landmarker = await this.landmarker();
      if (generation !== this.generation) return;
      await this.refreshDevices();
      const activeDeviceId = stream.getVideoTracks()[0]?.getSettings().deviceId ?? options.cameraDeviceId;
      this.publish({ activeDeviceId, phase: "running", error: "" });
      this.tick(generation, landmarker, options);
    } catch (error) {
      if (generation !== this.generation) return;
      this.landmarkerPromise = null;
      this.stopCapture();
      const message = error instanceof Error ? error.message : String(error);
      const denied = error instanceof DOMException && (error.name === "NotAllowedError" || error.name === "SecurityError");
      this.publish({ phase: denied ? "denied" : "error", error: message, point: null, pose: "none", confidence: 0 });
    }
  }

  stop(): void {
    this.generation += 1;
    this.configKey = "";
    this.wanted = null;
    this.stopCapture();
    this.publish({ ...INITIAL_STATE, devices: this.state.devices });
  }

  async refreshDevices(): Promise<void> {
    if (!navigator.mediaDevices?.enumerateDevices) return;
    try {
      const devices = (await navigator.mediaDevices.enumerateDevices())
        .filter((device) => device.kind === "videoinput")
        .map((device, index) => ({ deviceId: device.deviceId, label: device.label || `Camera ${index + 1}` }));
      this.publish({ devices });
    } catch {
      this.publish({ devices: [] });
    }
  }

  private watchEnvironment(): void {
    if (this.watching) return;
    this.watching = true;
    document.addEventListener("visibilitychange", () => {
      if (!this.wanted) return;
      if (!document.hidden) {
        if (this.state.phase === "off") void this.start(this.wanted);
        return;
      }
      if (this.state.phase !== "requesting" && this.state.phase !== "loading" && this.state.phase !== "running") return;
      this.generation += 1;
      this.configKey = "";
      this.stopCapture();
      this.publish({ ...IDLE_HAND, phase: "off", error: "" });
    });
    navigator.mediaDevices?.addEventListener?.("devicechange", () => {
      if (this.wanted) void this.refreshDevices();
    });
  }

  private tick(generation: number, landmarker: HandLandmarker, options: GestureRecognitionOptions): void {
    if (generation !== this.generation || !this.video) return;
    const now = performance.now();
    if (this.lastFrameAt > 0 && now - this.lastFrameAt > STALE_FRAME_MS && (this.state.pose !== "none" || this.state.point || this.state.palmPoint)) {
      this.lastLandmarks = null;
      this.engine.reset();
      this.publish(IDLE_HAND);
    }
    if (this.video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA && this.video.currentTime !== this.lastVideoTime) {
      this.lastVideoTime = this.video.currentTime;
      this.lastFrameAt = now;
      let result: HandLandmarkerResult;
      try {
        result = landmarker.detectForVideo(this.video, now);
      } catch (error) {
        this.stopCapture();
        this.publish({
          phase: "error",
          error: error instanceof Error ? error.message : String(error),
          point: null,
          pose: "none",
          confidence: 0,
        });
        return;
      }
      const landmarks = result.landmarks[0];
      this.lastLandmarks = landmarks ?? null;
      if (landmarks) {
        const recognition = this.engine.process(landmarks, now, options);
        const confidence = result.handedness[0]?.[0]?.score ?? 0;
        if (recognition.event) emitAirHandGesture(recognition.event, "camera");
        if (
          recognition.pose !== this.state.pose
          || recognition.point || this.state.point
          || recognition.palmPoint || this.state.palmPoint
          || recognition.pinchRatio != null || this.state.pinchRatio != null
          || now - this.lastPublishedAt >= 100
        ) {
          this.lastPublishedAt = now;
          this.publish({
            point: recognition.point,
            palmPoint: recognition.palmPoint,
            pinchRatio: recognition.pinchRatio,
            curls: recognition.curls,
            pose: recognition.pose,
            confidence,
          });
        }
      } else if (this.state.pose !== "none" || this.state.point || this.state.palmPoint) {
        this.engine.reset();
        this.publish(IDLE_HAND);
      }
    }
    this.drawPreview();
    this.frame = window.requestAnimationFrame(() => this.tick(generation, landmarker, options));
  }

  private drawPreview(): void {
    const canvas = this.preview;
    if (!canvas) return;
    const width = canvas.clientWidth;
    const height = canvas.clientHeight;
    if (!width || !height) return;
    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    const deviceWidth = Math.round(width * ratio);
    const deviceHeight = Math.round(height * ratio);
    if (canvas.width !== deviceWidth || canvas.height !== deviceHeight) {
      canvas.width = deviceWidth;
      canvas.height = deviceHeight;
    }
    const context = canvas.getContext("2d");
    if (!context) return;
    context.clearRect(0, 0, canvas.width, canvas.height);
    const video = this.video;
    if (!video || video.readyState < HTMLMediaElement.HAVE_CURRENT_DATA || !video.videoWidth) return;
    const mirror = this.lastOptions?.mirrorX !== false;
    const scale = Math.max(canvas.width / video.videoWidth, canvas.height / video.videoHeight);
    const drawWidth = video.videoWidth * scale;
    const drawHeight = video.videoHeight * scale;
    const offsetX = (canvas.width - drawWidth) / 2;
    const offsetY = (canvas.height - drawHeight) / 2;
    context.save();
    if (mirror) {
      context.translate(canvas.width, 0);
      context.scale(-1, 1);
    }
    context.drawImage(video, offsetX, offsetY, drawWidth, drawHeight);
    const landmarks = this.lastLandmarks;
    if (landmarks) {
      const pointFor = (index: number) => {
        const mark = landmarks[index];
        return mark ? { x: offsetX + mark.x * drawWidth, y: offsetY + mark.y * drawHeight } : null;
      };
      context.lineWidth = 2 * ratio;
      context.lineCap = "round";
      context.strokeStyle = "rgb(255 255 255 / 82%)";
      for (const [from, to] of HAND_CONNECTIONS) {
        const start = pointFor(from);
        const end = pointFor(to);
        if (!start || !end) continue;
        context.beginPath();
        context.moveTo(start.x, start.y);
        context.lineTo(end.x, end.y);
        context.stroke();
      }
      context.fillStyle = "#fff";
      landmarks.forEach((_, index) => {
        const point = pointFor(index);
        if (!point) return;
        const radius = (index === 4 || index === 8 ? 3.2 : 2.1) * ratio;
        context.beginPath();
        context.arc(point.x, point.y, radius, 0, Math.PI * 2);
        context.fill();
      });
    }
    context.restore();
  }

  private async landmarker(): Promise<HandLandmarker> {
    if (!this.landmarkerPromise) {
      const wasmRoot = new URL("mediapipe/wasm", document.baseURI).toString().replace(/\/$/, "");
      const modelPath = new URL("models/hand_landmarker.task", document.baseURI).toString();
      this.landmarkerPromise = import("@mediapipe/tasks-vision").then(async ({ FilesetResolver, HandLandmarker }) => {
        const vision = await FilesetResolver.forVisionTasks(wasmRoot);
        return HandLandmarker.createFromOptions(vision, {
          baseOptions: { delegate: "CPU", modelAssetPath: modelPath },
          minHandDetectionConfidence: 0.5,
          minHandPresenceConfidence: 0.5,
          minTrackingConfidence: 0.5,
          numHands: 1,
          runningMode: "VIDEO",
        });
      });
    }
    return this.landmarkerPromise;
  }

  private stopCapture(): void {
    if (this.frame) window.cancelAnimationFrame(this.frame);
    this.frame = 0;
    this.lastVideoTime = -1;
    this.lastFrameAt = 0;
    this.lastPublishedAt = 0;
    this.lastLandmarks = null;
    this.engine.reset();
    if (this.video) {
      this.video.pause();
      this.video.srcObject = null;
      this.video.remove();
      this.video = null;
    }
    if (this.stream) {
      stopStream(this.stream);
      this.stream = null;
    }
    this.drawPreview();
  }

  private publish(patch: Partial<GestureRecognitionState>): void {
    this.state = Object.freeze({ ...this.state, ...patch });
    this.updateCoarse();
    this.updateDevicesView();
    for (const subscriber of [...this.subscribers]) subscriber();
  }

  private updateCoarse(): void {
    const state = this.state;
    const cached = this.coarse;
    const structural = state.phase !== cached.phase
      || state.pose !== cached.pose
      || state.error !== cached.error
      || state.devices !== cached.devices
      || state.activeDeviceId !== cached.activeDeviceId;
    const now = performance.now();
    if (structural || now - this.coarseAt >= 150) {
      this.coarse = state;
      this.coarseAt = now;
    }
  }

  private updateDevicesView(): void {
    const state = this.state;
    const cached = this.devicesView;
    if (state.devices === cached.devices
      && state.phase === cached.phase
      && state.error === cached.error
      && state.activeDeviceId === cached.activeDeviceId) {
      return;
    }
    this.devicesView = {
      devices: state.devices,
      phase: state.phase,
      error: state.error,
      activeDeviceId: state.activeDeviceId,
    };
  }
}

const controller = new GestureRecognitionController();

export function useGestureRecognitionState(): GestureRecognitionState {
  return useSyncExternalStore(controller.subscribe, controller.snapshot, () => INITIAL_STATE);
}

export function useGestureRecognitionCoarseState(): GestureRecognitionState {
  return useSyncExternalStore(controller.subscribe, controller.coarseSnapshot, () => INITIAL_STATE);
}

const INITIAL_DEVICES_VIEW: GestureDevicesView = {
  devices: INITIAL_STATE.devices,
  phase: INITIAL_STATE.phase,
  error: INITIAL_STATE.error,
  activeDeviceId: INITIAL_STATE.activeDeviceId,
};

export function useGestureDevices(): GestureDevicesView {
  return useSyncExternalStore(controller.subscribe, controller.devicesSnapshot, () => INITIAL_DEVICES_VIEW);
}

export function subscribeGestureState(listener: (state: GestureRecognitionState) => void): () => void {
  return controller.subscribe(() => listener(controller.snapshot()));
}

export function currentGestureState(): GestureRecognitionState {
  return controller.snapshot();
}

export function startGestureRecognition(options: GestureRecognitionOptions): Promise<void> {
  return controller.start(options);
}

export function stopGestureRecognition(): void {
  controller.stop();
}

export function refreshGestureCameras(): Promise<void> {
  return controller.refreshDevices();
}

export function attachGesturePreview(canvas: HTMLCanvasElement | null): void {
  controller.attachPreview(canvas);
}

export type CameraPermission = { granted: boolean; status: string };

export async function readCameraPermission(): Promise<CameraPermission> {
  const transport = window.octopusTransport;
  if (transport?.cameraAccessStatus) return transport.cameraAccessStatus();
  return { granted: true, status: "browser-managed" };
}

export async function requestCameraPermission(): Promise<CameraPermission> {
  const transport = window.octopusTransport;
  if (transport?.requestCameraAccess) return transport.requestCameraAccess();
  return { granted: true, status: "browser-managed" };
}

async function openCamera(deviceId: string): Promise<MediaStream> {
  const request = (exactDeviceId: string) => navigator.mediaDevices.getUserMedia({
    audio: false,
    video: {
      deviceId: exactDeviceId ? { exact: exactDeviceId } : undefined,
      facingMode: "user",
      frameRate: { ideal: 30, max: 30 },
      height: { ideal: 720 },
      width: { ideal: 1280 },
    },
  });
  try {
    return await request(deviceId);
  } catch (error) {
    const missing = error instanceof DOMException && (error.name === "OverconstrainedError" || error.name === "NotFoundError");
    if (!deviceId || !missing) throw error;
    return request("");
  }
}

function stopStream(stream: MediaStream): void {
  for (const track of stream.getTracks()) track.stop();
}
