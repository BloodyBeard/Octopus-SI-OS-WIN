import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import type { OctopusClientAccessProfile } from "../../electron";
import { transcribeDictation } from "../../projectionClient";
import { applyDictationDictionary, useClientPreferences, useSetClientPreferences } from "../../clientPreferences";
import { encodeVoiceRecording } from "./voiceRecorderEncoding";

type DictationAction = "insert" | "send";
type DictationError = "microphone-missing" | "permission-denied" | "unavailable" | "unsupported" | "transcription";
type DictationHandlers = {
  getSurroundingText: () => string;
  onTranscriptInsert: (transcript: string) => void;
  onTranscriptSend: (transcript: string) => void;
};
type RetryableDictation = { action: DictationAction; audio: Blob; durationMs: number; handlers: DictationHandlers };

const MIN_RECORDING_MS = 250;
const TRANSCRIPTION_DELAY_MS = 650;
const WAVEFORM_BAR_STEP_PX = 6;
const WAVEFORM_BAR_WIDTH_PX = 3;
const WAVEFORM_STEP_MS = 200;
const WAVEFORM_SAMPLE_FLOOR = 0.0025;
const WAVEFORM_NOISE_FLOOR = 0.006;
const WAVEFORM_SAMPLE_CEILING = 0.16;
const TRANSCRIPTION_SAMPLE_RATE = 24_000;

export function useLocalDictation({
  accessProfile,
  getSurroundingText,
  onError,
  onTranscriptInsert,
  onTranscriptSend,
}: {
  accessProfile: OctopusClientAccessProfile;
  getSurroundingText: () => string;
  onError: (message: string) => void;
  onTranscriptInsert: (transcript: string) => void;
  onTranscriptSend: (transcript: string) => void;
}) {
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  const [isDictating, setIsDictating] = useState(false);
  const [isTranscribing, setIsTranscribing] = useState(false);
  const [canRetryDictation, setCanRetryDictation] = useState(false);
  const waveformCanvasRef = useRef<HTMLCanvasElement>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const stopActionRef = useRef<DictationAction>("insert");
  const stopRequestedRef = useRef<DictationAction | "abort" | null>(null);
  const retryRef = useRef<RetryableDictation | null>(null);
  const startedAtRef = useRef(0);
  const mountedRef = useRef(true);
  const onErrorRef = useRef(onError);
  const handlersRef = useRef<DictationHandlers>({ getSurroundingText, onTranscriptInsert, onTranscriptSend });
  const waveformRef = useRef<WaveformCapture | null>(null);
  onErrorRef.current = onError;
  handlersRef.current = { getSurroundingText, onTranscriptInsert, onTranscriptSend };

  const stopCapture = useCallback(() => {
    streamRef.current?.getTracks().forEach((track) => track.stop());
    streamRef.current = null;
    waveformRef.current?.stop();
    waveformRef.current = null;
  }, []);

  const transcribe = useCallback(async (attempt: RetryableDictation) => {
    if (!mountedRef.current) return;
    setIsTranscribing(true);
    try {
      const rawTranscript = accessProfile === "lab"
        ? await simulatedDictation(attempt)
        : (await transcribeDictation({
            bytes: await attempt.audio.arrayBuffer(),
            durationMs: attempt.durationMs,
            mediaType: "audio/wav",
            provider: preferences.dictationProvider,
            sampleRateHz: TRANSCRIPTION_SAMPLE_RATE,
            timeoutMs: 120_000,
          })).text.trim();
      const transcript = applyDictationDictionary(rawTranscript, preferences.dictationDictionary).trim();
      if (!mountedRef.current) return;
      retryRef.current = null;
      setCanRetryDictation(false);
      if (transcript) {
        const createdAtMs = Date.now();
        setClientPreferences({
          dictationHistory: [{
            id: `dictation_${createdAtMs.toString(36)}_${crypto.getRandomValues(new Uint32Array(1))[0]?.toString(36)}`,
            text: transcript,
            createdAtMs,
          }, ...preferences.dictationHistory].slice(0, 30),
        });
        if (attempt.action === "send") attempt.handlers.onTranscriptSend(transcript);
        else attempt.handlers.onTranscriptInsert(transcript);
      }
    } catch (failure) {
      retryRef.current = attempt;
      setCanRetryDictation(true);
      onErrorRef.current(failure instanceof Error ? failure.message : dictationErrorMessage("transcription"));
    } finally {
      if (mountedRef.current) setIsTranscribing(false);
    }
  }, [accessProfile, preferences.dictationDictionary, preferences.dictationHistory, preferences.dictationProvider, setClientPreferences]);

  const finishRecording = useCallback(async () => {
    const duration = Math.max(0, performance.now() - startedAtRef.current);
    const requested = stopRequestedRef.current;
    const action = requested === "send" ? "send" : stopActionRef.current;
    const recording = requested === "abort" ? null : waveformRef.current?.encode() ?? null;
    recorderRef.current = null;
    stopRequestedRef.current = null;
    stopCapture();
    if (!mountedRef.current) return;
    setIsDictating(false);
    if (requested === "abort" || duration < MIN_RECORDING_MS) return;
    if (!recording) {
      onErrorRef.current("The recording did not contain any audio samples");
      return;
    }
    const audio = new Blob([recording.bytes], { type: "audio/wav" });
    await transcribe({ action, audio, durationMs: recording.durationMs, handlers: handlersRef.current });
  }, [stopCapture, transcribe]);

  const stopDictation = useCallback((action: DictationAction) => {
    stopRequestedRef.current = stopRequestedRef.current === "send" ? "send" : action;
    stopActionRef.current = action;
    const recorder = recorderRef.current;
    if (!recorder || recorder.state === "inactive") {
      void finishRecording();
      return;
    }
    recorder.stop();
  }, [finishRecording]);

  const abortDictation = useCallback(() => {
    stopRequestedRef.current = "abort";
    const recorder = recorderRef.current;
    if (!recorder || recorder.state === "inactive") {
      stopCapture();
      setIsDictating(false);
      return;
    }
    recorder.stop();
  }, [stopCapture]);

  const startDictation = useCallback(async () => {
    if (isDictating || isTranscribing) return;
    retryRef.current = null;
    setCanRetryDictation(false);
    stopRequestedRef.current = null;
    if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
      onErrorRef.current(dictationErrorMessage("unsupported"));
      return;
    }
    try {
      const stream = await requestMicrophone(accessProfile === "lab" ? "default" : preferences.audioInputDeviceId);
      if (!mountedRef.current) {
        stream.getTracks().forEach((track) => track.stop());
        return;
      }
      const recorder = new MediaRecorder(stream);
      streamRef.current = stream;
      recorderRef.current = recorder;
      stopActionRef.current = "insert";
      recorder.onstop = () => void finishRecording();
      recorder.start();
      startedAtRef.current = performance.now();
      setIsDictating(true);
      waveformRef.current = startWaveform(stream, waveformCanvasRef);
    } catch (failure) {
      stopCapture();
      stopRequestedRef.current = null;
      setIsDictating(false);
      onErrorRef.current(dictationErrorMessage(classifyStartError(failure)));
    }
  }, [accessProfile, finishRecording, isDictating, isTranscribing, preferences.audioInputDeviceId, stopCapture]);

  const retryDictation = useCallback(() => {
    if (isDictating || isTranscribing || !canRetryDictation || !retryRef.current) return;
    setCanRetryDictation(false);
    void transcribe(retryRef.current);
  }, [canRetryDictation, isDictating, isTranscribing, transcribe]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      const recorder = recorderRef.current;
      if (recorder) {
        recorder.onstop = null;
        if (recorder.state !== "inactive") recorder.stop();
      }
      stopRequestedRef.current = "abort";
      stopCapture();
    };
  }, [stopCapture]);

  return {
    abortDictation,
    canRetryDictation,
    isDictating,
    isTranscribing,
    retryDictation,
    startDictation,
    stopDictation,
    waveformCanvasRef,
  };
}

function simulatedTranscript(audio: Blob, surroundingText: string): string {
  if (audio.size <= 0) throw new Error("The recording is empty");
  const fixture = "Review the active run and continue with the next implementation step.";
  const tail = surroundingText.trim().toLocaleLowerCase();
  return tail.endsWith(fixture.toLocaleLowerCase()) ? "" : fixture;
}

async function simulatedDictation(attempt: RetryableDictation): Promise<string> {
  await delay(TRANSCRIPTION_DELAY_MS);
  return simulatedTranscript(attempt.audio, attempt.handlers.getSurroundingText()).trim();
}

type WaveformCapture = { encode: () => ReturnType<typeof encodeVoiceRecording>; stop: () => void };

function startWaveform(stream: MediaStream, canvasRef: RefObject<HTMLCanvasElement | null>): WaveformCapture {
  const context = new AudioContext();
  const source = context.createMediaStreamSource(stream);
  const processor = context.createScriptProcessor(2048, 1, 1);
  const recordingChunks: Float32Array[] = [];
  let values: number[] = [];
  let canvasWidth = 0;
  let frame = 0;
  let stopped = false;
  let scrollProgress = 0;
  let summedLevel = 0;
  let levelCount = 0;
  let signalBars = 0;
  let lastFrameAt = performance.now();

  const resetForCanvas = (width: number) => {
    const bars = Math.max(1, Math.floor(width / WAVEFORM_BAR_STEP_PX) + 1);
    const tail = values.slice(-bars);
    values = [
      ...Array.from({ length: Math.max(0, bars - tail.length) }, () => WAVEFORM_SAMPLE_FLOOR),
      ...tail,
    ];
    signalBars = Math.min(signalBars, bars);
    canvasWidth = width;
  };

  const draw = () => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const width = canvas.clientWidth;
    const height = canvas.clientHeight;
    if (!width || !height) return;
    if (values.length === 0 || width !== canvasWidth) resetForCanvas(width);
    const ratio = window.devicePixelRatio || 1;
    const nextWidth = Math.round(width * ratio);
    const nextHeight = Math.round(height * ratio);
    if (canvas.width !== nextWidth) canvas.width = nextWidth;
    if (canvas.height !== nextHeight) canvas.height = nextHeight;
    const drawing = canvas.getContext("2d");
    if (!drawing) return;
    drawing.setTransform(1, 0, 0, 1, 0, 0);
    drawing.clearRect(0, 0, canvas.width, canvas.height);
    drawing.save();
    const center = canvas.height * 0.5;
    drawing.translate(0, center);
    drawing.fillStyle = getComputedStyle(canvas).color || "#000";
    const barWidth = WAVEFORM_BAR_WIDTH_PX * ratio;
    const barStep = WAVEFORM_BAR_STEP_PX * ratio;
    const offset = -barStep * scrollProgress;
    const signalStart = values.length - signalBars;
    values.forEach((value, index) => {
      const amplitude = Math.max(barWidth, (canvas.height - barWidth) * value);
      const x = canvas.width - (values.length - index - 1) * barStep + offset;
      drawing.globalAlpha = index < signalStart ? 0.2 : 0.5;
      drawing.beginPath();
      drawing.roundRect(x, -amplitude / 2, barWidth, amplitude, barWidth / 2);
      drawing.fill();
    });
    drawing.globalCompositeOperation = "destination-in";
    drawing.globalAlpha = 1;
    const edge = drawing.createLinearGradient(0, 0, canvas.width, 0);
    const edgeStop = Math.min(0.5, barStep / canvas.width);
    edge.addColorStop(0, "transparent");
    edge.addColorStop(edgeStop, "black");
    edge.addColorStop(1 - edgeStop, "black");
    edge.addColorStop(1, "transparent");
    drawing.fillStyle = edge;
    drawing.fillRect(0, -center, canvas.width, canvas.height);
    drawing.restore();
  };

  const animate = (now: number) => {
    if (stopped) return;
    scrollProgress += (now - lastFrameAt) / WAVEFORM_STEP_MS;
    lastFrameAt = now;
    const steps = Math.floor(scrollProgress);
    if (steps > 0) {
      const level = normalizeWaveformLevel(summedLevel / Math.max(1, levelCount));
      const count = Math.min(steps, values.length);
      for (let index = 0; index < count; index += 1) {
        values.push(level);
        values.shift();
      }
      signalBars = Math.min(values.length, signalBars + count);
      summedLevel = 0;
      levelCount = 0;
      scrollProgress -= steps;
    }
    draw();
    frame = window.requestAnimationFrame(animate);
  };

  frame = window.requestAnimationFrame(animate);
  processor.onaudioprocess = (event) => {
    const input = event.inputBuffer.getChannelData(0);
    recordingChunks.push(new Float32Array(input));
    let squares = 0;
    for (let index = 0; index < input.length; index += 1) {
      const sample = input[index] ?? 0;
      squares += sample * sample;
    }
    if (values.length === 0 && canvasRef.current?.clientWidth) resetForCanvas(canvasRef.current.clientWidth);
    summedLevel += Math.sqrt(squares / Math.max(1, input.length));
    levelCount += 1;
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
      void context.close();
    },
  };
}

function normalizeWaveformLevel(value: number): number {
  const signal = Math.max(0, value - WAVEFORM_NOISE_FLOOR);
  return Math.min(1, signal / (WAVEFORM_SAMPLE_CEILING - WAVEFORM_NOISE_FLOOR)) ** 0.6;
}

async function requestMicrophone(deviceId: string) {
  const audio = deviceId && deviceId !== "default"
    ? { channelCount: 1, deviceId: { exact: deviceId } }
    : { channelCount: 1 };
  try {
    return await navigator.mediaDevices.getUserMedia({ audio });
  } catch (error) {
    if (!(error instanceof DOMException) || error.name !== "NotSupportedError") throw error;
    const device = (await navigator.mediaDevices.enumerateDevices()).find(
      (candidate) => candidate.kind === "audioinput" && candidate.deviceId && candidate.deviceId !== "default",
    );
    if (!device) throw error;
    return navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, deviceId: { exact: device.deviceId } } });
  }
}

function classifyStartError(error: unknown): DictationError {
  const name = error instanceof DOMException ? error.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") return "permission-denied";
  if (name === "NotFoundError" || name === "DevicesNotFoundError") return "microphone-missing";
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

function delay(ms: number) {
  return new Promise<void>((resolve) => window.setTimeout(resolve, ms));
}
