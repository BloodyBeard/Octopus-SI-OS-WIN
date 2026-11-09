import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import type { OctopusClientAccessProfile, OctopusDictationResponse } from "../../electron";
import { transcribeDictation } from "../../projectionClient";
import { applyDictationDictionary, useClientPreferences, useSetClientPreferences } from "../../clientPreferences";
import { dictationAudioRequest, dictationHost, dictationMessage, dictationResult, notifyDictationRecordingsChanged } from "../../clientDictationRecordings";
import { encodeVoiceRecording } from "./voiceRecorderEncoding";
import dictationLimits from "../../../electron/clientDictationLimits.json";

type DictationError = "microphone-missing" | "permission-denied" | "unavailable" | "unsupported" | "transcription";
type RetryableDictation = { audio: Blob; durationMs: number; recordingId: string | null; action: "insert" | "send" };
const MIN_RECORDING_MS = 250;
const TRANSCRIPTION_SAMPLE_RATE = 24_000;

export function useLocalDictation({ accessProfile, composerId, onTranscriptStart, onTranscriptUpdate, onTranscriptCancel, onTranscriptInsert, onTranscriptSend }: {
  accessProfile: OctopusClientAccessProfile;
  composerId: string;
  onTranscriptStart: () => void;
  onTranscriptUpdate: (transcript: string) => void;
  onTranscriptCancel: () => void;
  onTranscriptInsert: (transcript: string) => void;
  onTranscriptSend: (transcript: string) => void;
}) {
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  const [phase, setPhase] = useState<"idle" | "starting" | "recording" | "transcribing">("idle");
  const [error, setError] = useState("");
  const [storageWarning, setStorageWarning] = useState("");
  const [lastRecordingId, setLastRecordingId] = useState<string | null>(null);
  const [retryAudio, setRetryAudio] = useState<Blob | null>(null);
  const waveformCanvasRef = useRef<HTMLCanvasElement>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const waveformRef = useRef<WaveformCapture | null>(null);
  const retryRef = useRef<RetryableDictation | null>(null);
  const mountedRef = useRef(true);
  const phaseRef = useRef(phase);
  const operationRef = useRef(0);
  const requestRef = useRef("");
  const contextRef = useRef<AudioContext | null>(null);
  const streamCleanupRef = useRef<(() => void) | null>(null);
  const stopRef = useRef<(action?: "insert" | "send", failure?: Error) => void>(() => {});
  const handlersRef = useRef({ onTranscriptStart, onTranscriptUpdate, onTranscriptCancel });
  handlersRef.current = { onTranscriptStart, onTranscriptUpdate, onTranscriptCancel };
  const preferencesRef = useRef(preferences);
  const insertRef = useRef(onTranscriptInsert);
  const sendRef = useRef(onTranscriptSend);
  preferencesRef.current = preferences;
  insertRef.current = onTranscriptInsert;
  sendRef.current = onTranscriptSend;

  const changePhase = useCallback((next: typeof phase) => {
    phaseRef.current = next;
    if (mountedRef.current) setPhase(next);
  }, []);
  const stopCapture = useCallback(() => {
    streamRef.current?.getTracks().forEach((track) => track.stop());
    streamRef.current = null;
    waveformRef.current?.stop();
    waveformRef.current = null;
    if (contextRef.current?.state !== "closed") void contextRef.current?.close();
    contextRef.current = null;
  }, []);

  const transcribe = useCallback(async (attempt: RetryableDictation, operation: number, responsePromise?: Promise<OctopusDictationResponse>) => {
    const responseOutcome = responsePromise?.then((value) => ({ value }), (failure: unknown) => ({ failure }));
    changePhase("transcribing");
    setError("");
    const current = () => mountedRef.current && operationRef.current === operation;
    try {
      if (accessProfile === "lab") throw new Error("Transcription is unavailable in Test Lab. Use the Administrator profile to dictate.");
      if (!attempt.recordingId) {
        try {
          const saved = await dictationResult(dictationHost().saveDictationRecording(dictationAudioRequest(await attempt.audio.arrayBuffer(), attempt.durationMs)));
          attempt.recordingId = saved?.id ?? null;
          notifyDictationRecordingsChanged();
          if (current()) setLastRecordingId(attempt.recordingId);
        } catch { if (current()) setStorageWarning("Couldn't save the audio. It is available for retry while this composer stays open."); }
      }
      let response;
      if (responseOutcome) {
        const outcome = await responseOutcome;
        if ("failure" in outcome) throw outcome.failure;
        response = await dictationResult(Promise.resolve(outcome.value));
      } else {
        const request = dictationAudioRequest(await attempt.audio.arrayBuffer(), attempt.durationMs);
        requestRef.current = request.attemptId;
        response = await dictationResult(transcribeDictation(request));
      }
      if (!current()) return;
      if (!current()) return;
      const transcript = applyDictationDictionary(response.text, preferencesRef.current.dictationDictionary).trim();
      if (!transcript) throw new Error("No speech was recognized. Try again or record a new dictation.");
      retryRef.current = null;
      setRetryAudio(null);
      const createdAtMs = Date.now();
      setClientPreferences({ dictationHistory: [{ id: attempt.recordingId ?? crypto.randomUUID(), text: transcript, createdAtMs }, ...preferencesRef.current.dictationHistory].slice(0, 30) });
      if (attempt.recordingId) {
        try {
          await dictationResult(dictationHost().updateDictationRecording({ id: attempt.recordingId, text: transcript, status: "transcribed" }));
          notifyDictationRecordingsChanged();
        } catch { if (current()) setStorageWarning("The audio was saved, but its transcript could not be saved. You can still copy it from the message field."); }
      }
      if (current()) {
        if (attempt.action === "send") sendRef.current(transcript);
        else insertRef.current(transcript);
      }
    } catch (failure) {
      if (!current()) return;
      handlersRef.current.onTranscriptCancel();
      retryRef.current = attempt;
      setRetryAudio(attempt.audio);
      setError(dictationMessage(failure));
      if (attempt.recordingId) {
        try {
          await dictationResult(dictationHost().updateDictationRecording({ id: attempt.recordingId, text: "", status: "failed" }));
          notifyDictationRecordingsChanged();
        } catch { if (current()) setStorageWarning("Couldn't update the saved recording. The audio is still available while this composer is open."); }
      }
    } finally {
      if (current()) { requestRef.current = ""; changePhase("idle"); }
    }
  }, [accessProfile, changePhase, setClientPreferences]);

  const stopDictation = useCallback((action: "insert" | "send" = "insert", failure?: Error) => {
    if (phaseRef.current !== "recording") return;
    changePhase("transcribing");
    const operation = operationRef.current;
    const recording = waveformRef.current?.encode();
    stopCapture();
    streamCleanupRef.current?.();
    streamCleanupRef.current = null;
    const attemptId = requestRef.current;
    if (!recording || recording.durationMs < MIN_RECORDING_MS) {
      void dictationHost().cancelDictation(attemptId).catch(() => undefined);
      requestRef.current = "";
      handlersRef.current.onTranscriptCancel();
      changePhase("idle");
      setError("Didn't catch that. Record a little longer and try again.");
      return;
    }
    const attempt: RetryableDictation = { audio: new Blob([recording.bytes], { type: "audio/wav" }), durationMs: recording.durationMs, recordingId: null, action };
    const response = failure ? Promise.reject(failure) : dictationHost().finishDictationStream(attemptId);
    if (failure) void dictationHost().cancelDictation(attemptId).catch(() => undefined);
    void transcribe(attempt, operation, response);
  }, [changePhase, stopCapture, transcribe]);
  stopRef.current = stopDictation;

  const abortDictation = useCallback(() => {
    operationRef.current += 1;
    const recording = phaseRef.current === "recording" ? waveformRef.current?.encode() : null;
    if (recording && recording.durationMs >= MIN_RECORDING_MS) {
      void dictationResult(dictationHost().saveDictationRecording(dictationAudioRequest(recording.bytes, recording.durationMs)))
        .then(notifyDictationRecordingsChanged, () => { if (mountedRef.current) setStorageWarning("Couldn't save the cancelled recording."); });
    }
    stopCapture();
    streamCleanupRef.current?.();
    streamCleanupRef.current = null;
    handlersRef.current.onTranscriptCancel();
    retryRef.current = null;
    if (requestRef.current) {
      void dictationHost().cancelDictation(requestRef.current).catch(() => undefined);
      requestRef.current = "";
    }
    if (mountedRef.current) {
      setError("");
      setStorageWarning("");
      setRetryAudio(null);
      setLastRecordingId(null);
    }
    changePhase("idle");
  }, [changePhase, stopCapture]);

  const startDictation = useCallback(async () => {
    if (phaseRef.current !== "idle") return;
    abortDictation();
    const operation = operationRef.current;
    if (!navigator.mediaDevices?.getUserMedia || typeof AudioContext === "undefined") {
      setError(dictationErrorMessage("unsupported"));
      return;
    }
    if (accessProfile === "lab") {
      setError("Transcription is unavailable in Test Lab. Use the Administrator profile to dictate.");
      return;
    }
    changePhase("starting");
    handlersRef.current.onTranscriptStart();
    let microphoneReady = false;
    try {
      const host = dictationHost();
      if (!host.startDictationStream) throw new Error("Streaming dictation requires the updated desktop client.");
      const stream = await requestMicrophone(preferencesRef.current.audioInputDeviceId);
      if (!mountedRef.current || operationRef.current !== operation) {
        stream.getTracks().forEach((track) => track.stop());
        return;
      }
      microphoneReady = true;
      streamRef.current = stream;
      const context = new AudioContext({ sampleRate: TRANSCRIPTION_SAMPLE_RATE });
      contextRef.current = context;
      const attemptId = crypto.randomUUID();
      requestRef.current = attemptId;
      streamCleanupRef.current = host.onDictationStreamEvent((event) => {
        if (!mountedRef.current || operationRef.current !== operation || event.attemptId !== attemptId) return;
        if (event.type === "transcript") handlersRef.current.onTranscriptUpdate(event.text);
        else if (event.type === "stop-requested") stopRef.current();
        else if (event.type === "error" && phaseRef.current === "recording") stopRef.current("insert", Object.assign(new Error(event.error.message), { code: event.error.code }));
      });
      await dictationResult(host.startDictationStream({ attemptId, sampleRateHz: context.sampleRate }));
      if (!mountedRef.current || operationRef.current !== operation) return;
      if (context.state === "suspended") await context.resume();
      if (!mountedRef.current || operationRef.current !== operation) return;
      waveformRef.current = startWaveform(stream, context, waveformCanvasRef, () => stopRef.current(), (bytes) => host.appendDictationAudio(attemptId, bytes));
      changePhase("recording");
    } catch (failure) {
      if (!mountedRef.current || operationRef.current !== operation) return;
      stopCapture();
      streamCleanupRef.current?.();
      streamCleanupRef.current = null;
      if (requestRef.current) void dictationHost().cancelDictation(requestRef.current).catch(() => undefined);
      requestRef.current = "";
      handlersRef.current.onTranscriptCancel();
      changePhase("idle");
      setError(microphoneReady || failure instanceof Error && !(failure instanceof DOMException) ? dictationMessage(failure) : dictationErrorMessage(classifyStartError(failure)));
    }
  }, [abortDictation, accessProfile, changePhase, stopCapture]);

  const retryDictation = useCallback(() => {
    if (phaseRef.current !== "idle" || !retryRef.current) return;
    void transcribe(retryRef.current, ++operationRef.current);
  }, [transcribe]);

  useEffect(() => {
    mountedRef.current = true;
    setPhase(phaseRef.current);
    return () => { mountedRef.current = false; abortDictation(); };
  }, [abortDictation, accessProfile, composerId]);

  return { abortDictation, canRetryDictation: Boolean(retryAudio), error, storageWarning, lastRecordingId,
    isStarting: phase === "starting", isDictating: phase === "recording", isTranscribing: phase === "transcribing",
    retryDictation, startDictation, stopDictation, waveformCanvasRef };
}

type WaveformCapture = { encode: () => ReturnType<typeof encodeVoiceRecording>; stop: () => void };

function startWaveform(stream: MediaStream, context: AudioContext, canvasRef: RefObject<HTMLCanvasElement | null>, onLimit: () => void, onAudio: (bytes: ArrayBuffer) => void): WaveformCapture {
  const source = context.createMediaStreamSource(stream);
  const processor = context.createScriptProcessor(2048, 1, 1);
  const recordingChunks: Float32Array[] = [];
  const maxSamples = Math.floor(context.sampleRate * dictationLimits.maxDurationMs / 1_000);
  let sampleCount = 0;
  let frame = 0;
  let stopped = false;
  let lastFrameAt = performance.now();
  const waveform = new DictationWaveform();
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const animate = (now: number) => {
    if (stopped) return;
    waveform.advance((now - lastFrameAt) / 1_000, now / 1_000, reducedMotion);
    lastFrameAt = now;
    if (canvasRef.current) waveform.draw(canvasRef.current);
    frame = window.requestAnimationFrame(animate);
  };
  frame = window.requestAnimationFrame(animate);
  processor.onaudioprocess = (event) => {
    if (stopped || sampleCount >= maxSamples) return;
    const input = event.inputBuffer.getChannelData(0).subarray(0, maxSamples - sampleCount);
    recordingChunks.push(new Float32Array(input));
    sampleCount += input.length;
    const pcm = new Int16Array(input.length);
    let squares = 0;
    for (let index = 0; index < input.length; index += 1) {
      const sample = input[index] ?? 0;
      squares += sample * sample;
      const clamped = Math.max(-1, Math.min(1, sample));
      pcm[index] = clamped < 0 ? clamped * 32768 : clamped * 32767;
    }
    const rms = Math.sqrt(squares / Math.max(1, input.length));
    waveform.receive(Math.min(1, Math.max(0, rms - 0.006) / 0.154) ** 0.6, performance.now() / 1_000);
    onAudio(pcm.buffer);
    event.outputBuffer?.getChannelData(0).fill(0);
    if (sampleCount === maxSamples) onLimit();
  };
  source.connect(processor);
  processor.connect(context.destination);
  return {
    encode: () => encodeVoiceRecording(recordingChunks, context.sampleRate, TRANSCRIPTION_SAMPLE_RATE),
    stop: () => {
      stopped = true;
      window.cancelAnimationFrame(frame);
      processor.onaudioprocess = null;
      processor.disconnect();
      source.disconnect();
    },
  };
}

class DictationWaveform {
  levels = Array<number>(6).fill(0);
  audioLevels = Array<number>(6).fill(0);
  inputLevel = 0;
  inputTime = -Infinity;
  elapsed = 0;
  receive(level: number, time: number) { this.inputLevel = level; this.inputTime = time; }
  advance(seconds: number, now: number, reducedMotion: boolean) {
    const elapsed = Math.min(Math.max(seconds, 0), 1 / 15);
    this.elapsed += elapsed;
    const input = now - this.inputTime <= 0.3 ? this.inputLevel : 0;
    const weights = [0.64, 0.88, 1, 1, 0.82, 0.58];
    for (let index = 0; index < this.levels.length; index += 1) {
      const modulation = reducedMotion ? 0.8 : 0.72 + 0.28 * Math.sin(this.elapsed * (4.6 + index * 0.71) + index * 1.67);
      const target = Math.min(Math.max(input * weights[index] * modulation, 0), 1);
      const current = this.audioLevels[index];
      this.audioLevels[index] += (target - current) * (1 - Math.exp(-elapsed / (target > current ? 0.04 : 0.16)));
      const time = this.elapsed * 2 - index * 0.055;
      const pulse = reducedMotion || time < 0 ? 0 : time < 0.18 ? 0.5 - 0.5 * Math.cos(Math.PI * time / 0.18) : time < 0.23 ? 1 : time < 0.53 ? 0.5 + 0.5 * Math.cos(Math.PI * (time - 0.23) / 0.3) : 0;
      this.levels[index] = Math.max(this.audioLevels[index], pulse);
    }
  }
  draw(canvas: HTMLCanvasElement) {
    const width = canvas.clientWidth;
    const height = canvas.clientHeight;
    const drawing = canvas.getContext("2d");
    if (!drawing || !width || !height) return;
    const ratio = (window.devicePixelRatio || 1) * (canvas.getBoundingClientRect().width / width);
    const nextWidth = Math.round(width * ratio), nextHeight = Math.round(height * ratio);
    if (canvas.width !== nextWidth) canvas.width = nextWidth;
    if (canvas.height !== nextHeight) canvas.height = nextHeight;
    drawing.setTransform(ratio, 0, 0, ratio, 0, 0);
    drawing.clearRect(0, 0, width, height);
    drawing.fillStyle = getComputedStyle(canvas).color;
    this.levels.forEach((level, index) => {
      const amplitude = 3 + level * (Math.min(height, 20) - 3);
      drawing.beginPath();
      drawing.roundRect((width - 27) / 2 + index * 5, (height - amplitude) / 2, 2, amplitude, 1);
      drawing.fill();
    });
  }
}

async function requestMicrophone(deviceId: string) {
  const audio = deviceId && deviceId !== "default"
    ? { channelCount: 1, deviceId: { exact: deviceId } }
    : { channelCount: 1 };
  return navigator.mediaDevices.getUserMedia({ audio });
}

function classifyStartError(error: unknown): DictationError {
  const name = error instanceof DOMException ? error.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") return "permission-denied";
  if (name === "NotFoundError" || name === "DevicesNotFoundError" || name === "OverconstrainedError" || name === "ConstraintNotSatisfiedError") return "microphone-missing";
  if (name === "NotReadableError" || name === "TrackStartError") return "unavailable";
  return "unavailable";
}

function dictationErrorMessage(error: DictationError): string {
  if (error === "permission-denied") return "Allow microphone access to use dictation";
  if (error === "microphone-missing") return "Connect a microphone to use dictation";
  if (error === "unavailable") return "Close other apps using the microphone";
  if (error === "unsupported") return "Dictation is not available on this device";
  return "Unable to transcribe audio";
}
