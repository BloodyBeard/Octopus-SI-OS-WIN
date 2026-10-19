import { useCallback, useEffect, useRef, useState } from "react";
import type { OctopusClientAccessProfile } from "../../electron";
import {
  abortInterfaceArtifactStream,
  appendInterfaceArtifactStream,
  beginInterfaceArtifactStream,
  finalizeInterfaceArtifactStream,
  readProjection,
} from "../../projectionClient";
import { nextProjectionChange } from "../../projectionLive";
import { useClientPreferences, useSetClientPreferences } from "../../clientPreferences";
import { isVoiceCallActive, voiceTranscriptActivity, type VoiceTranscriptEntry } from "./voicePresentation";
import { createVoiceTranscript } from "./voiceTranscript";
import { assertVoiceControlReady, canRetireVoiceCall, closeVoiceCall, deliverVoiceRecording, openVoiceCall, readVoiceArtifacts, updateVoiceContext, voiceContextDelta, type VoiceCall, type VoiceCallBinding, type VoiceContextMessage, type VoiceContextSelection } from "./voiceCallArtifacts";

export type VoicePhase = "idle" | "requesting" | "connecting" | "listening" | "muted" | "recovering" | "ending" | "closure_unconfirmed" | "ended" | "error";

export type VoiceSessionContext = VoiceCallBinding & {
  homeRunId: string;
  runTitle: string;
  providerLabel: string;
};

export type VoiceSessionStartOptions = {
  saveRecording?: boolean;
  context?: VoiceContextSelection;
};

type InterfaceArtifactRow = {
  artifact_id: string;
  label: string;
  seq: number;
  payload?: Record<string, unknown>;
  error?: string;
};

type InterfaceMediaPayload = {
  artifact_id: string;
  run_id: string;
  interface_id: string;
  correlation_id: string;
  mime_type: string;
  content_hash: string;
  size: number;
  base64: string;
};

export type PendingVoiceAudioArtifact = {
  artifactId: string;
  mimeType: string;
  size: number;
};

const BAR_COUNT = 36;
const FRAME_INTERVAL_MS = 34;
const REALTIME_CONNECTION_TIMEOUT_MS = 15_000;

export type VoiceMeterSnapshot = {
  level: number;
  waveform: number[];
};

export type VoiceDeviceOption = {
  deviceId: string;
  label: string;
};

export function useVoiceSession(accessProfile: OctopusClientAccessProfile) {
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  const [phase, setPhase] = useState<VoicePhase>("idle");
  const [error, setError] = useState("");
  const [transcript, setTranscript] = useState<readonly VoiceTranscriptEntry[]>([]);
  const [waitingForReply, setWaitingForReply] = useState(false);
  const waitingForReplyRef = useRef(false);
  const [pendingContextCount, setPendingContextCount] = useState(0);
  const callRef = useRef<VoiceCall | null>(null);
  const captionsRef = useRef<ReturnType<typeof createVoiceTranscript> | null>(null);
  const stoppingRef = useRef<Promise<void> | null>(null);
  const mutedRef = useRef(false);
  const providerReadyRef = useRef(false);
  const deviceGenerationRef = useRef(0);
  const recoveryTimerRef = useRef<number | null>(null);
  const [startedAtMs, setStartedAtMs] = useState(0);
  const [context, setContext] = useState<VoiceSessionContext | null>(null);
  const [inputDevices, setInputDevices] = useState<VoiceDeviceOption[]>([]);
  const [outputDevices, setOutputDevices] = useState<VoiceDeviceOption[]>([]);
  const [selectedInputId, setSelectedInputId] = useState(preferences.audioInputDeviceId);
  const [selectedOutputId, setSelectedOutputId] = useState(preferences.audioOutputDeviceId);
  const [deviceError, setDeviceError] = useState("");
  const [pendingAudioArtifact, setPendingAudioArtifact] = useState<PendingVoiceAudioArtifact | null>(null);
  const [meter] = useState(createVoiceMeterStore);
  const [outputMeter] = useState(createVoiceMeterStore);
  const streamRef = useRef<MediaStream | null>(null);
  const audioContextRef = useRef<AudioContext | null>(null);
  const sourceRef = useRef<MediaStreamAudioSourceNode | null>(null);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const peerConnectionRef = useRef<RTCPeerConnection | null>(null);
  const remoteAudioRef = useRef<HTMLAudioElement | null>(null);
  const artifactAudioRef = useRef<HTMLAudioElement | null>(null);
  const remoteStreamRef = useRef<MediaStream | null>(null);
  const recordingRef = useRef<VoiceRecordingController | null>(null);
  const outputMeterConnectionRef = useRef<VoiceOutputMeterConnection | null>(null);
  const saveRecordingRef = useRef(false);
  const selectedOutputRef = useRef("");
  const speakerEnabledRef = useRef(true);
  const animationFrameRef = useRef<number | null>(null);
  const generationRef = useRef(0);
  const scopeGenerationRef = useRef(0);
  const mountedRef = useRef(false);
  const startupAbortRef = useRef<AbortController | null>(null);
  const inputChangeRef = useRef<Promise<void>>(Promise.resolve());
  const outputChangeRef = useRef<Promise<void>>(Promise.resolve());
  const enumerationRef = useRef(0);
  const callContextRef = useRef<VoiceContextSelection | null>(null);
  const sharedContextRef = useRef(new Map<string, VoiceContextMessage>());
  const pendingContextRef = useRef(new Map<string, VoiceContextMessage>());
  const contextPumpRef = useRef(false);
  const contextFailureRef = useRef(new Set<string>());
  const contextUpdateRef = useRef<Promise<void>>(Promise.resolve());
  const incomingAudioBehaviorRef = useRef(preferences.incomingAudioArtifactBehavior);
  const pendingAudioDecisionRef = useRef<{
    artifactId: string;
    resolve: (play: boolean) => void;
  } | null>(null);
  const startedAtRef = useRef(0);
  const lastFrameAtRef = useRef(0);

  const resolvePendingAudioArtifact = useCallback((play: boolean) => {
    const decision = pendingAudioDecisionRef.current;
    if (!decision) return;
    pendingAudioDecisionRef.current = null;
    setPendingAudioArtifact(null);
    decision.resolve(play);
  }, []);

  const authorizeIncomingAudioArtifact = useCallback((media: InterfaceMediaPayload): Promise<boolean> => {
    if (incomingAudioBehaviorRef.current === "play") return Promise.resolve(true);
    if (incomingAudioBehaviorRef.current === "never") return Promise.resolve(false);
    resolvePendingAudioArtifact(false);
    return new Promise((resolve) => {
      pendingAudioDecisionRef.current = { artifactId: media.artifact_id, resolve };
      setPendingAudioArtifact({ artifactId: media.artifact_id, mimeType: media.mime_type, size: media.size });
    });
  }, [resolvePendingAudioArtifact]);

  const refreshDevices = useCallback(async () => {
    if (accessProfile === "lab") {
      const inputs = [{ deviceId: "test-microphone", label: "Test microphone" }];
      const outputs = [{ deviceId: "test-speaker", label: "Test speaker" }];
      setInputDevices(inputs);
      setOutputDevices(outputs);
      setSelectedInputId("test-microphone");
      setSelectedOutputId("test-speaker");
      setDeviceError("");
      return;
    }
    if (!navigator.mediaDevices?.enumerateDevices) return;
    const scope = scopeGenerationRef.current;
    const enumeration = ++enumerationRef.current;
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      if (!mountedRef.current || scope !== scopeGenerationRef.current || enumeration !== enumerationRef.current) return;
      const inputs = normalizeDevices(devices.filter((device) => device.kind === "audioinput"), "Microphone");
      const outputs = normalizeDevices(devices.filter((device) => device.kind === "audiooutput"), "Speaker");
      setInputDevices(inputs);
      setOutputDevices(outputs);
      if (!callRef.current) {
        setSelectedInputId((current) => chooseDeviceId(current, inputs));
        setSelectedOutputId((current) => chooseDeviceId(current, outputs));
      }
    } catch (failure) {
      if (!mountedRef.current || scope !== scopeGenerationRef.current || enumeration !== enumerationRef.current) return;
      setDeviceError(failure instanceof Error && failure.message ? failure.message : "Audio devices could not be listed");
    }
  }, [accessProfile]);

  const releaseCapture = useCallback((recordingDisposition: "deliver" | "discard" = "discard") => {
    waitingForReplyRef.current = false;
    setWaitingForReply(false);
    setPendingContextCount(0);
    resolvePendingAudioArtifact(false);
    startupAbortRef.current?.abort();
    startupAbortRef.current = null;
    if (recoveryTimerRef.current !== null) window.clearTimeout(recoveryTimerRef.current);
    recoveryTimerRef.current = null;
    deviceGenerationRef.current += 1;
    const recording = recordingRef.current;
    recordingRef.current = null;
    if (recording) {
      const generation = generationRef.current;
      const scope = scopeGenerationRef.current;
      void recording.finish(recordingDisposition === "deliver").catch((failure) => {
        if (!mountedRef.current || generation !== generationRef.current || scope !== scopeGenerationRef.current) return;
        setDeviceError(`Voice recording could not be delivered: ${errorText(failure)}`);
      });
    }
    if (animationFrameRef.current != null) window.cancelAnimationFrame(animationFrameRef.current);
    animationFrameRef.current = null;
    analyserRef.current?.disconnect();
    analyserRef.current = null;
    sourceRef.current?.disconnect();
    sourceRef.current = null;
    streamRef.current?.getTracks().forEach((track) => track.stop());
    streamRef.current = null;
    remoteStreamRef.current = null;
    outputMeterConnectionRef.current?.dispose();
    outputMeterConnectionRef.current = null;
    outputMeter.set(quietMeter());
    if (remoteAudioRef.current) {
      remoteAudioRef.current.pause();
      remoteAudioRef.current.srcObject = null;
      remoteAudioRef.current.remove();
      remoteAudioRef.current = null;
    }
    if (artifactAudioRef.current) {
      artifactAudioRef.current.pause();
      artifactAudioRef.current.src = "";
      artifactAudioRef.current.remove();
      artifactAudioRef.current = null;
    }
    const audioContext = audioContextRef.current;
    audioContextRef.current = null;
    if (audioContext && audioContext.state !== "closed") void audioContext.close();
  }, [outputMeter, resolvePendingAudioArtifact]);

  const releaseConnection = useCallback((peer: RTCPeerConnection | null) => {
    peer?.close();
    if (peerConnectionRef.current === peer) peerConnectionRef.current = null;
  }, []);

  const finishCall = useCallback((failure?: string | Error): Promise<void> => {
    if (stoppingRef.current) return stoppingRef.current;
    const call = callRef.current;
    if (!call) return Promise.resolve();
    const captions = captionsRef.current;
    const peer = peerConnectionRef.current;
    const scope = scopeGenerationRef.current;
    generationRef.current += 1;
    releaseCapture("deliver");
    meter.set(quietMeter());
    providerReadyRef.current = false;
    setPhase("ending");
    const stopping = (async () => {
      let message = failure === undefined ? "" : typeof failure === "string" ? failure : microphoneError(failure);
      try {
        if (call) await closeVoiceCall(call);
        if (callRef.current === call && scope === scopeGenerationRef.current && Array.isArray(call.closedPayload?.transcript)) {
          for (const entry of call.closedPayload.transcript) {
            if (!entry || typeof entry !== "object" || Array.isArray(entry)) throw new Error("Voice closure has a malformed transcript.");
            const snapshot = captions?.artifact({ ...entry, final: entry.complete });
            if (snapshot) setTranscript(snapshot);
          }
        }
      } catch (closeFailure) {
        if (closeFailure !== failure) message = [message, errorText(closeFailure)].filter(Boolean).join(" ");
      } finally {
        releaseConnection(peer);
      }
      if (!mountedRef.current || callRef.current !== call || scope !== scopeGenerationRef.current) return;
      const unresolved = !canRetireVoiceCall(call);
      if (!unresolved) callRef.current = null;
      if (unresolved && !message) message = "Your microphone is off, but the provider has not confirmed call closure.";
      if (message) void window.octopusTransport?.logClientEvent?.({
        level: "error", event: "renderer.voice_closure", message: `${call.correlationId}: ${message}`,
      }).catch(() => undefined);
      setError(message);
      setPhase(unresolved ? "closure_unconfirmed" : message ? "error" : "ended");
      saveRecordingRef.current = false;
    })();
    stoppingRef.current = stopping;
    const clearStopping = () => { if (stoppingRef.current === stopping) stoppingRef.current = null; };
    void stopping.then(clearStopping, clearStopping);
    return stopping;
  }, [meter, releaseCapture, releaseConnection]);

  const end = useCallback(() => finishCall(), [finishCall]);

  const beginCapture = useCallback(async (nextContext: VoiceSessionContext, inputDeviceId: string, saveRecording: boolean, selection?: VoiceContextSelection) => {
    const generation = ++generationRef.current;
    const startupAbort = new AbortController();
    startupAbortRef.current = startupAbort;
    inputChangeRef.current = Promise.resolve();
    outputChangeRef.current = Promise.resolve();
    contextUpdateRef.current = Promise.resolve();
    callContextRef.current = selection ?? null;
    sharedContextRef.current = new Map(selection?.messages.map((message) => [message.id, message]) ?? []);
    pendingContextRef.current = new Map();
    contextPumpRef.current = false;
    contextFailureRef.current = new Set();
    const current = () => generation === generationRef.current;
    const call: VoiceCall = { binding: nextContext, correlationId: cryptoRandomId(), answer: null, control: null, stop: null, closeRequested: false, creationDisposition: "not_created", callId: "", remoteClosed: false };
    callRef.current = call;
    const captions = createVoiceTranscript();
    captionsRef.current = captions;
    setContext(nextContext);
    setError("");
    setDeviceError("");
    setTranscript([]);
    setStartedAtMs(0);
    startedAtRef.current = 0;
    mutedRef.current = false;
    providerReadyRef.current = false;
    meter.set(quietMeter());
    setPhase("requesting");
    if (accessProfile === "live") recordVoiceStartup(call, "checking control");

    if (accessProfile === "lab") {
      startedAtRef.current = performance.now();
      setStartedAtMs(startedAtRef.current);
      setPhase("listening");
      return;
    }

    const updateConnection = () => {
      if (!current()) return;
      const peer = peerConnectionRef.current;
      const ready = startedAtRef.current > 0 && providerReadyRef.current && peer?.connectionState === "connected";
      for (const track of streamRef.current?.getAudioTracks() ?? []) track.enabled = ready && !mutedRef.current;
      if (!ready) {
        meter.set(quietMeter());
        if (startedAtRef.current) setPhase("recovering");
        return;
      }
      if (recoveryTimerRef.current !== null) window.clearTimeout(recoveryTimerRef.current);
      recoveryTimerRef.current = null;
      if (startedAtRef.current) setPhase(mutedRef.current ? "muted" : "listening");
    };

    try {
      if (!navigator.mediaDevices?.getUserMedia || typeof AudioContext === "undefined" || typeof RTCPeerConnection === "undefined") {
        throw new Error("Voice input is not available on this device.");
      }
      await assertVoiceControlReady(nextContext);
      if (!current()) return;
      recordVoiceStartup(call, "control ready");
      const nativePermission = await window.octopusTransport?.requestMicrophoneAccess();
      if (!current()) return;
      if (nativePermission && !nativePermission.granted) throw new MicrophonePermissionError(nativePermission.status);
      recordVoiceStartup(call, "microphone permission granted");
      const stream = await requestMicrophone(inputDeviceId);
      if (!current()) { stream.getTracks().forEach((track) => track.stop()); return; }
      recordVoiceStartup(call, "microphone acquired");
      if (!stream.getAudioTracks().some((track) => track.readyState === "live")) {
        stream.getTracks().forEach((track) => track.stop());
        throw new Error("The microphone has no live audio track.");
      }
      streamRef.current = stream;
      for (const track of stream.getAudioTracks()) {
        track.enabled = false;
        track.addEventListener("ended", () => {
          if (current() && streamRef.current === stream) void finishCall("The microphone was disconnected.");
        }, { once: true });
      }
      setPhase("connecting");

      const peer = new RTCPeerConnection();
      peerConnectionRef.current = peer;
      peer.createDataChannel("oai-events");
      for (const track of stream.getAudioTracks()) peer.addTrack(track, stream);
      peer.ontrack = (event) => {
        if (!current()) return;
        const remoteStream = event.streams[0] ?? new MediaStream([event.track]);
        remoteStreamRef.current = remoteStream;
        recordingRef.current?.connectRemote(remoteStream);
        const audio = remoteAudioRef.current ?? document.createElement("audio");
        audio.autoplay = false;
        audio.hidden = true;
        audio.muted = !speakerEnabledRef.current;
        audio.srcObject = remoteStream;
        if (!remoteAudioRef.current) { document.body.append(audio); remoteAudioRef.current = audio; }
        void applyOutputDevice(audio, selectedOutputRef.current).then(() => {
          if (current() && remoteAudioRef.current === audio) return audio.play();
        }).catch((failure) => {
          if (current()) setDeviceError(`Speaker playback failed: ${errorText(failure)}`);
        });
        const audioContext = audioContextRef.current;
        if (audioContext && outputMeterConnectionRef.current?.kind !== "artifact") {
          outputMeterConnectionRef.current?.dispose();
          outputMeterConnectionRef.current = createStreamOutputMeter(audioContext, remoteStream, outputMeter, generation, generationRef);
        }
      };
      peer.onconnectionstatechange = () => {
        if (!current()) return;
        if (peer.connectionState === "failed" || peer.connectionState === "closed") {
          void finishCall(`Voice connection ${peer.connectionState}.`);
          return;
        }
        updateConnection();
        if (peer.connectionState === "disconnected" && recoveryTimerRef.current === null) {
          recoveryTimerRef.current = window.setTimeout(() => {
            if (current() && peer.connectionState !== "connected") void finishCall("Voice connection could not recover.");
          }, REALTIME_CONNECTION_TIMEOUT_MS);
        }
      };
      const offer = await peer.createOffer({ offerToReceiveAudio: true, offerToReceiveVideo: false });
      if (!current()) return;
      await peer.setLocalDescription(offer);
      if (!current()) return;
      await waitForIceGathering(peer, startupAbort.signal);
      if (!current()) return;
      const offerSdp = requiredSessionDescription(peer.localDescription?.sdp, "Voice produced no WebRTC offer.");
      const answer = await openVoiceCall(call, offerSdp, selection?.initialItems);
      if (!current()) return;
      if (answer.sideband_connected !== true) throw new Error("Voice provider control connection is not ready.");
      providerReadyRef.current = true;
      await peer.setRemoteDescription({ type: "answer", sdp: requiredSessionDescription(answer.answer_sdp, "Voice provider returned no answer SDP.") });
      await waitForPeerConnectionConnected(peer, generation, generationRef, REALTIME_CONNECTION_TIMEOUT_MS, startupAbort.signal);
      if (!current()) return;

      const audioContext = new AudioContext();
      audioContextRef.current = audioContext;
      await audioContext.resume();
      if (!current()) { if (audioContext.state !== "closed") await audioContext.close(); return; }
      const source = audioContext.createMediaStreamSource(stream);
      const analyser = audioContext.createAnalyser();
      analyser.fftSize = 512;
      analyser.smoothingTimeConstant = 0.72;
      source.connect(analyser);
      sourceRef.current = source;
      analyserRef.current = analyser;
      if (saveRecording) {
        const recording = await createVoiceRecording({ audioContext, call, microphoneSource: source, startedAt: new Date().toISOString() });
        if (!current()) { await recording.finish(false); return; }
        recordingRef.current = recording;
        if (remoteStreamRef.current) recording.connectRemote(remoteStreamRef.current);
      }
      if (remoteStreamRef.current) {
        outputMeterConnectionRef.current?.dispose();
        outputMeterConnectionRef.current = createStreamOutputMeter(audioContext, remoteStreamRef.current, outputMeter, generation, generationRef);
      }
      startedAtRef.current = performance.now();
      setStartedAtMs(startedAtRef.current);
      lastFrameAtRef.current = 0;
      updateConnection();
      const samples = new Uint8Array(analyser.fftSize);
      const draw = (now: number) => {
        if (!current() || !analyserRef.current) return;
        animationFrameRef.current = window.requestAnimationFrame(draw);
        if (now - lastFrameAtRef.current < FRAME_INTERVAL_MS) return;
        lastFrameAtRef.current = now;
        if (!streamRef.current?.getAudioTracks().some((track) => track.enabled)) return;
        analyserRef.current.getByteTimeDomainData(samples);
        const next = waveformFromSamples(samples);
        meter.set({ level: next.level, waveform: next.bars });
      };
      animationFrameRef.current = window.requestAnimationFrame(draw);
      void (async () => {
        let afterSeq = -1;
        let previousTranscript: readonly VoiceTranscriptEntry[] = [];
        while (current()) {
          const changed = nextProjectionChange(500);
          const batch = await readVoiceArtifacts(call, ["realtime.transcript", "realtime.session.state", "realtime.session.closed"], afterSeq);
          if (!current()) return;
          afterSeq = batch.afterSeq;
          for (const artifact of batch.artifacts) {
            const payload = artifact.payload!;
            if (payload.call_id !== call.callId) continue;
            if (artifact.label === "realtime.transcript") {
              const previous = previousTranscript.find(entry => entry.id === payload.id);
              previousTranscript = captions.artifact(payload);
              setTranscript(previousTranscript);
              const activity = voiceTranscriptActivity(previous, payload);
              if (activity !== null) {
                waitingForReplyRef.current = activity;
                setWaitingForReply(waitingForReplyRef.current);
              }
            }
            if (artifact.label === "realtime.session.state") {
              if (payload.state === "failed") throw new Error(typeof payload.reason === "string" ? payload.reason : "Voice provider connection failed.");
              if (payload.state === "reconnecting") providerReadyRef.current = false;
              if (payload.state === "attached") providerReadyRef.current = true;
              updateConnection();
            }
            if (artifact.label === "realtime.session.closed") {
              call.remoteClosed = payload.remote_closed === true;
              call.closedPayload = payload;
              await finishCall(call.remoteClosed ? undefined : "Voice provider did not confirm call closure.");
              return;
            }
          }
          if (!batch.more) await changed;
        }
      })().catch((failure) => { if (current()) void finishCall(errorText(failure)); });
      void pumpVoiceAudioArtifacts({ artifactAudioRef, authorizePlayback: authorizeIncomingAudioArtifact, correlationId: call.correlationId, generation, generationRef, interfaceId: nextContext.providerId, audioContextRef, outputDeviceId: selectedOutputRef, outputMeter, outputMeterConnectionRef, remoteStreamRef, runId: nextContext.runId, speakerEnabled: speakerEnabledRef })
        .catch((failure) => { if (current()) void finishCall(`Voice audio artifact failed: ${errorText(failure)}`); });
      void refreshDevices();
    } catch (failure) {
      if (current()) await finishCall(failure instanceof Error ? failure : microphoneError(failure));
    }
  }, [accessProfile, authorizeIncomingAudioArtifact, finishCall, meter, outputMeter, refreshDevices]);

  const start = useCallback(async (nextContext: VoiceSessionContext, options: VoiceSessionStartOptions = {}) => {
    if (!mountedRef.current || callRef.current || stoppingRef.current || isVoiceCallActive(phase)) return;
    saveRecordingRef.current = accessProfile === "live" && options.saveRecording === true;
    if (!nextContext.homeRunId || nextContext.homeRunId === nextContext.runId
      || (options.context && options.context.runId !== nextContext.homeRunId)) {
      setError("Voice context belongs to a different Home run.");
      setPhase("error");
      return;
    }
    await beginCapture(nextContext, selectedInputId, saveRecordingRef.current, options.context);
  }, [accessProfile, beginCapture, phase, selectedInputId]);

  const updateContext = useCallback(async (selection: VoiceContextSelection) => {
    const call = callRef.current;
    const signal = startupAbortRef.current?.signal;
    if (!call?.callId || !signal || !providerReadyRef.current || peerConnectionRef.current?.connectionState !== "connected" || (phase !== "listening" && phase !== "muted")) return;
    if (selection.runId !== callContextRef.current?.runId || selection.interfaceId !== callContextRef.current?.interfaceId) return;
    const generation = generationRef.current;
    const shared = sharedContextRef.current;
    const pending = pendingContextRef.current;
    try {
      for (const message of selection.messages) {
        if (contextFailureRef.current.has(message.id)) continue;
        if (voiceContextDelta(pending.get(message.id) ?? shared.get(message.id), message)) pending.set(message.id, message);
      }
      if (pending.size > 24) throw new Error("Voice has more than 24 unconfirmed Home updates.");
    } catch (failure) {
      setError(errorText(failure));
      return;
    }
    if (!pending.size) return;
    if (contextPumpRef.current) return contextUpdateRef.current;
    contextPumpRef.current = true;
    setPendingContextCount(pending.size);
    const operation = contextUpdateRef.current.then(async () => {
      let sendingId = "";
      try {
        while (pending.size) {
          if (generation !== generationRef.current || signal.aborted || call.closeRequested) return;
          const message = pending.values().next().value!;
          pending.delete(message.id);
          sendingId = message.id;
          const delta = voiceContextDelta(shared.get(message.id), message);
          if (!delta) continue;
          await updateVoiceContext(call, delta, signal);
          shared.set(message.id, message);
        }
      } catch (failure) {
        if (generation === generationRef.current) {
          contextFailureRef.current.add(sendingId);
          pending.delete(sendingId);
          setError(`Voice did not confirm this Home context update: ${errorText(failure)} This and any remaining messages were not confirmed; no automatic retry was made.`);
        }
      } finally {
        if (generation === generationRef.current) {
          contextPumpRef.current = false;
          setPendingContextCount(0);
        }
      }
    });
    contextUpdateRef.current = operation;
    await operation;
  }, [phase]);

  const selectInputDevice = useCallback(async (deviceId: string) => {
    if (!callRef.current || accessProfile === "lab") {
      setSelectedInputId(deviceId);
      setClientPreferences({ audioInputDeviceId: deviceId });
      return;
    }
    if (phase !== "listening" && phase !== "muted") return;
    const generation = generationRef.current;
    const deviceGeneration = ++deviceGenerationRef.current;
    const current = () => generation === generationRef.current;
    const selected = () => current() && deviceGeneration === deviceGenerationRef.current;
    const operation = inputChangeRef.current.then(async () => {
      if (!selected()) return;
      let replacement: MediaStream | null = null;
      let source: MediaStreamAudioSourceNode | null = null;
      let sender: RTCRtpSender | undefined;
      let previousTrack: MediaStreamTrack | null = null;
      let replaced = false;
      let committed = false;
      try {
        replacement = await requestMicrophone(deviceId);
        for (const track of replacement.getAudioTracks()) track.enabled = false;
        if (!selected()) return;
        const track = replacement.getAudioTracks()[0];
        if (!track || track.readyState !== "live") throw new Error("Selected device has no live audio track.");
        const peer = peerConnectionRef.current;
        sender = peer?.getSenders().find((item) => item.track?.kind === "audio");
        const audioContext = audioContextRef.current;
        const analyser = analyserRef.current;
        if (!sender || !audioContext || !analyser) throw new Error("The active call has no microphone sender.");
        previousTrack = sender.track;
        source = audioContext.createMediaStreamSource(replacement);
        source.connect(analyser);
        await sender.replaceTrack(track);
        replaced = true;
        if (!selected()) return;
        if (track.readyState !== "live") throw new Error("The selected microphone was disconnected.");
        recordingRef.current?.replaceMicrophone(source);
        const previous = streamRef.current;
        sourceRef.current?.disconnect();
        sourceRef.current = source;
        streamRef.current = replacement;
        committed = true;
        track.enabled = providerReadyRef.current && peer?.connectionState === "connected" && !mutedRef.current;
        const selectedStream = replacement;
        track.addEventListener("ended", () => { if (current() && streamRef.current === selectedStream) void finishCall("The microphone was disconnected."); }, { once: true });
        previous?.getTracks().forEach((item) => item.stop());
        setSelectedInputId(deviceId);
        setClientPreferences({ audioInputDeviceId: deviceId });
        setDeviceError("");
      } catch (failure) {
        if (selected()) setDeviceError(errorText(failure));
      } finally {
        if (!committed) {
          if (replaced && sender && current()) {
            try { await sender.replaceTrack(previousTrack); }
            catch (failure) { if (current()) await finishCall(`Microphone replacement could not be rolled back: ${errorText(failure)}`); }
          }
          source?.disconnect();
          replacement?.getTracks().forEach((track) => track.stop());
        }
      }
    });
    inputChangeRef.current = operation;
    await operation;
  }, [accessProfile, finishCall, phase, setClientPreferences]);

  const selectOutputDevice = useCallback(async (deviceId: string) => {
    if (!callRef.current) {
      selectedOutputRef.current = deviceId;
      setSelectedOutputId(deviceId);
      setClientPreferences({ audioOutputDeviceId: deviceId });
      return;
    }
    const generation = generationRef.current;
    const operation = outputChangeRef.current.then(async () => {
      if (generation !== generationRef.current) return;
      const previous = selectedOutputRef.current;
      const elements = [remoteAudioRef.current, artifactAudioRef.current].filter((element): element is HTMLAudioElement => element !== null);
      try {
        for (const audio of elements) {
          await applyOutputDevice(audio, deviceId);
          if (generation !== generationRef.current) return;
          await audio.play();
          if (generation !== generationRef.current) return;
        }
        selectedOutputRef.current = deviceId;
        setSelectedOutputId(deviceId);
        setClientPreferences({ audioOutputDeviceId: deviceId });
        setDeviceError("");
      } catch (failure) {
        if (generation !== generationRef.current) return;
        setDeviceError(`Speaker selection failed: ${errorText(failure)}`);
        for (const audio of elements) {
          try { await applyOutputDevice(audio, previous); }
          catch (rollbackFailure) {
            if (generation === generationRef.current) setDeviceError(`Speaker selection could not be restored: ${errorText(rollbackFailure)}`);
          }
          if (generation !== generationRef.current) return;
        }
      }
    });
    outputChangeRef.current = operation;
    await operation;
  }, [setClientPreferences]);

  const setSpeakerEnabled = useCallback((enabled: boolean) => {
    speakerEnabledRef.current = enabled;
    if (remoteAudioRef.current) remoteAudioRef.current.muted = !enabled;
    if (artifactAudioRef.current) artifactAudioRef.current.muted = !enabled;
  }, []);

  const toggleMute = useCallback(() => {
    if (accessProfile === "lab") {
      if (phase !== "listening" && phase !== "muted") return;
      const muted = phase !== "muted";
      setPhase(muted ? "muted" : "listening");
      if (muted) meter.set(quietMeter());
      return;
    }
    const stream = streamRef.current;
    if (!stream || (phase !== "listening" && phase !== "muted")) return;
    const muted = !mutedRef.current;
    mutedRef.current = muted;
    const ready = providerReadyRef.current && peerConnectionRef.current?.connectionState === "connected";
    for (const track of stream.getAudioTracks()) track.enabled = ready && !muted;
    setPhase(ready ? muted ? "muted" : "listening" : "recovering");
    if (muted || !ready) meter.set(quietMeter());
  }, [accessProfile, meter, phase]);

  const dismissError = useCallback(() => {
    if (phase !== "error") return;
    setError("");
    setPhase("ended");
  }, [phase]);

  useEffect(() => {
    mountedRef.current = true;
    scopeGenerationRef.current += 1;
    const retained = callRef.current;
    setPhase(retained ? retained.stop ? "ending" : "closure_unconfirmed" : "idle");
    setContext(null);
    setTranscript([]);
    setError(retained ? "Your previous call's closure is unconfirmed. Retry closure before starting another call." : "");
    setDeviceError("");
    setStartedAtMs(0);
    return () => {
      mountedRef.current = false;
      scopeGenerationRef.current += 1;
      generationRef.current += 1;
      const call = callRef.current;
      const peer = peerConnectionRef.current;
      if (call && canRetireVoiceCall(call)) callRef.current = null;
      captionsRef.current = null;
      stoppingRef.current = null;
      releaseCapture("deliver");
      meter.set(quietMeter());
      startedAtRef.current = 0;
      providerReadyRef.current = false;
      if (call) void closeVoiceCall(call).then(() => {
        if (callRef.current !== call || !canRetireVoiceCall(call)) return;
        callRef.current = null;
        if (mountedRef.current) { setPhase("idle"); setError(""); }
      }, (failure) => {
        if (mountedRef.current && callRef.current === call) { setPhase("closure_unconfirmed"); setError(errorText(failure)); }
        else console.error("Voice call closure failed", errorText(failure));
      }).finally(() => releaseConnection(peer));
      else releaseConnection(peer);
    };
  }, [accessProfile, meter, releaseCapture, releaseConnection]);

  useEffect(() => {
    const call = callRef.current;
    if (accessProfile !== "live" || phase !== "closure_unconfirmed" || !call?.callId) return;
    const scope = scopeGenerationRef.current;
    let cancelled = false;
    const current = () => !cancelled && mountedRef.current && callRef.current === call && scopeGenerationRef.current === scope;
    void (async () => {
      let afterSeq = -1;
      while (current()) {
        const changed = nextProjectionChange(1_000);
        const batch = await readVoiceArtifacts(call, ["realtime.session.closed"], afterSeq);
        if (!current()) return;
        afterSeq = batch.afterSeq;
        const closed = batch.artifacts.find((artifact) => artifact.label === "realtime.session.closed"
          && artifact.payload?.call_id === call.callId && artifact.payload.remote_closed === true)?.payload;
        if (closed) {
          call.remoteClosed = true;
          call.closedPayload = closed;
          await stoppingRef.current;
          if (current()) await finishCall();
          return;
        }
        if (!batch.more) await changed;
      }
    })().catch((failure) => {
      if (current()) setError(`Your microphone is off. Call closure is still unconfirmed; reading its confirmation failed: ${errorText(failure)}`);
    });
    return () => { cancelled = true; };
  }, [accessProfile, finishCall, phase]);

  useEffect(() => {
    selectedOutputRef.current = selectedOutputId;
  }, [selectedOutputId]);

  useEffect(() => {
    incomingAudioBehaviorRef.current = preferences.incomingAudioArtifactBehavior;
    if (!pendingAudioDecisionRef.current || preferences.incomingAudioArtifactBehavior === "ask") return;
    resolvePendingAudioArtifact(preferences.incomingAudioArtifactBehavior === "play");
  }, [preferences.incomingAudioArtifactBehavior, resolvePendingAudioArtifact]);

  useEffect(() => {
    if (isVoiceCallActive(phase)) return;
    const input = chooseDeviceId(preferences.audioInputDeviceId, inputDevices);
    const output = chooseDeviceId(preferences.audioOutputDeviceId, outputDevices);
    if (input && input !== selectedInputId) setSelectedInputId(input);
    if (output && output !== selectedOutputId) setSelectedOutputId(output);
  }, [inputDevices, outputDevices, phase, preferences.audioInputDeviceId, preferences.audioOutputDeviceId, selectedInputId, selectedOutputId]);

  useEffect(() => {
    void refreshDevices();
    const mediaDevices = navigator.mediaDevices;
    if (!mediaDevices?.addEventListener) return;
    const refresh = () => void refreshDevices();
    mediaDevices.addEventListener("devicechange", refresh);
    return () => mediaDevices.removeEventListener("devicechange", refresh);
  }, [refreshDevices]);

  return {
    context,
    deviceError,
    dismissError,
    end,
    error,
    getMeterSnapshot: meter.getSnapshot,
    getOutputMeterSnapshot: outputMeter.getSnapshot,
    inputDevices,
    phase,
    providerActivity: (phase === "listening" || phase === "muted") && (waitingForReply || pendingContextCount > 0),
    outputDevices,
    pendingAudioArtifact,
    refreshDevices,
    respondToPendingAudioArtifact: resolvePendingAudioArtifact,
    selectInputDevice,
    selectOutputDevice,
    selectedInputId,
    selectedOutputId,
    setSpeakerEnabled,
    start,
    startedAtMs,
    subscribeMeter: meter.subscribe,
    subscribeOutputMeter: outputMeter.subscribe,
    toggleMute,
    transcript,
    updateContext,
  };
}

export type VoiceSession = ReturnType<typeof useVoiceSession>;

type VoiceRecordingController = {
  replaceMicrophone(source: MediaStreamAudioSourceNode): void;
  connectRemote(stream: MediaStream): void;
  finish(deliver: boolean): Promise<void>;
};

type VoiceOutputMeterConnection = {
  kind: "artifact" | "remote";
  dispose(): void;
  start?(): Promise<void>;
};

async function createVoiceRecording(input: {
  audioContext: AudioContext;
  call: VoiceCall;
  microphoneSource: MediaStreamAudioSourceNode;
  startedAt: string;
}): Promise<VoiceRecordingController> {
  if (typeof MediaRecorder === "undefined") throw new Error("Audio recording is not available on this device.");
  const destination = input.audioContext.createMediaStreamDestination();
  let microphoneSource = input.microphoneSource;
  microphoneSource.connect(destination);
  const format = selectRecordingFormat();
  let recorder: MediaRecorder;
  try {
    recorder = format.mimeType
      ? new MediaRecorder(destination.stream, { mimeType: format.mimeType })
      : new MediaRecorder(destination.stream);
  } catch (failure) {
    microphoneSource.disconnect(destination);
    destination.disconnect();
    throw failure;
  }
  const mediaType = recorder.mimeType || format.mimeType;
  if (!mediaType) {
    microphoneSource.disconnect(destination);
    destination.disconnect();
    throw new Error("The browser did not declare a recording media type.");
  }
  const extension = recordingExtension(mediaType);
  const handle = await beginInterfaceArtifactStream({
    extension,
    interfaceId: input.call.binding.controlInterfaceId,
    label: "realtime.session.recording",
    mediaType,
    payload: {
      schema: "octopus.voice_recording",
      schema_version: 1,
      correlation_id: input.call.correlationId,
      started_at: input.startedAt,
    },
    runId: input.call.binding.runId,
  }).catch((failure) => {
    microphoneSource.disconnect(destination);
    destination.disconnect();
    throw failure;
  });
  const remoteSources: MediaStreamAudioSourceNode[] = [];
  const remoteKeys = new Set<string>();
  let appendFailure: unknown = null;
  let appendQueue = Promise.resolve();
  let finishPromise: Promise<void> | null = null;
  const stopped = new Promise<void>((resolve, reject) => {
    recorder.addEventListener("stop", () => resolve(), { once: true });
    recorder.addEventListener("error", (event) => {
      reject((event as Event & { error?: DOMException }).error ?? new Error("The media recorder stopped unexpectedly."));
    }, { once: true });
  });
  recorder.addEventListener("dataavailable", (event) => {
    if (!event.data.size) return;
    appendQueue = appendQueue
      .then(async () => {
        const bytes = new Uint8Array(await event.data.arrayBuffer());
        await appendInterfaceArtifactStream({ streamId: handle.streamId, bytes });
      })
      .catch((failure) => {
        appendFailure ??= failure;
      });
  });
  try {
    recorder.start(1_000);
  } catch (failure) {
    await abortInterfaceArtifactStream({ streamId: handle.streamId }).catch(() => {});
    microphoneSource.disconnect(destination);
    destination.disconnect();
    throw failure;
  }

  function disconnectSources() {
    try {
      microphoneSource.disconnect(destination);
    } catch {
    }
    for (const source of remoteSources) source.disconnect();
    remoteSources.length = 0;
    destination.disconnect();
  }

  return {
    replaceMicrophone(source) {
      source.connect(destination);
      microphoneSource.disconnect(destination);
      microphoneSource = source;
    },
    connectRemote(stream) {
      const audioTracks = stream.getAudioTracks();
      if (!audioTracks.length) return;
      const key = `${stream.id}:${audioTracks.map((track) => track.id).join(":")}`;
      if (remoteKeys.has(key)) return;
      remoteKeys.add(key);
      const source = input.audioContext.createMediaStreamSource(stream);
      source.connect(destination);
      remoteSources.push(source);
    },
    finish(deliver) {
      if (finishPromise) return finishPromise;
      finishPromise = (async () => {
        try {
          if (recorder.state !== "inactive") recorder.stop();
          await stopped;
          await appendQueue;
          if (appendFailure) throw appendFailure;
          if (deliver) {
            await deliverVoiceRecording(input.call, () => finalizeInterfaceArtifactStream({ streamId: handle.streamId, timeoutMs: 45_000 }));
          } else {
            await abortInterfaceArtifactStream({ streamId: handle.streamId });
          }
        } catch (failure) {
          await abortInterfaceArtifactStream({ streamId: handle.streamId }).catch(() => {});
          throw failure;
        } finally {
          disconnectSources();
        }
      })();
      return finishPromise;
    },
  };
}

function selectRecordingFormat(): { extension: string; mimeType: string } {
  const formats = [
    { extension: "webm", mimeType: "audio/webm;codecs=opus" },
    { extension: "webm", mimeType: "audio/webm" },
    { extension: "mp4", mimeType: "audio/mp4" },
    { extension: "ogg", mimeType: "audio/ogg;codecs=opus" },
  ];
  return formats.find((format) => MediaRecorder.isTypeSupported(format.mimeType)) ?? { extension: "bin", mimeType: "" };
}

function recordingExtension(mediaType: string): string {
  const normalized = mediaType.toLowerCase();
  if (normalized.includes("webm")) return "webm";
  if (normalized.includes("mp4")) return "mp4";
  if (normalized.includes("ogg")) return "ogg";
  if (normalized.includes("wav")) return "wav";
  return "bin";
}

async function pumpVoiceAudioArtifacts(input: {
  audioContextRef: { current: AudioContext | null };
  artifactAudioRef: { current: HTMLAudioElement | null };
  authorizePlayback: (media: InterfaceMediaPayload) => Promise<boolean>;
  correlationId: string;
  generation: number;
  generationRef: { current: number };
  interfaceId: string;
  outputDeviceId: { current: string };
  outputMeter: ReturnType<typeof createVoiceMeterStore>;
  outputMeterConnectionRef: { current: VoiceOutputMeterConnection | null };
  remoteStreamRef: { current: MediaStream | null };
  runId: string;
  speakerEnabled: { current: boolean };
}): Promise<void> {
  let afterSeq = -1;
  const seen = new Set<string>();
  while (input.generation === input.generationRef.current) {
    const changed = nextProjectionChange(1_000);
    const response = await readProjection<{ artifacts?: InterfaceArtifactRow[] }>(
      "projection.interface_artifacts.list",
      {
        after_seq: afterSeq,
        interface_id: input.interfaceId,
        labels: ["realtime.session.audio"],
        limit: 500,
        run_id: input.runId,
      },
    );
    if (input.generation !== input.generationRef.current) return;
    for (const artifact of response.artifacts ?? []) {
      afterSeq = Math.max(afterSeq, artifact.seq);
      if (artifact.error) throw new Error(artifact.error);
      if (projectionText(artifact.payload?.correlation_id) !== input.correlationId) continue;
      if (seen.has(artifact.artifact_id)) continue;
      seen.add(artifact.artifact_id);
      const media = await readProjection<InterfaceMediaPayload>(
        "projection.interface_media.read",
        {
          artifact_id: artifact.artifact_id,
          interface_id: input.interfaceId,
          run_id: input.runId,
        },
      );
      if (input.generation !== input.generationRef.current) return;
      if (media.correlation_id !== input.correlationId) {
        throw new Error("Realtime audio correlation does not match the active Voice call.");
      }
      const shouldPlay = await input.authorizePlayback(media);
      if (input.generation !== input.generationRef.current) return;
      if (!shouldPlay) continue;
      await playInterfaceAudio(media, input);
      if (input.generation !== input.generationRef.current) return;
    }
    await changed;
  }
}

async function playInterfaceAudio(
  media: InterfaceMediaPayload,
  input: {
    audioContextRef: { current: AudioContext | null };
    artifactAudioRef: { current: HTMLAudioElement | null };
    generation: number;
    generationRef: { current: number };
    outputDeviceId: { current: string };
    outputMeter: ReturnType<typeof createVoiceMeterStore>;
    outputMeterConnectionRef: { current: VoiceOutputMeterConnection | null };
    remoteStreamRef: { current: MediaStream | null };
    speakerEnabled: { current: boolean };
  },
): Promise<void> {
  if (!media.mime_type.startsWith("audio/")) throw new Error("Realtime media is not audio.");
  const binary = atob(media.base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  if (bytes.byteLength !== media.size) throw new Error("Realtime audio size changed after verification.");
  const url = URL.createObjectURL(new Blob([bytes], { type: media.mime_type }));
  const audio = document.createElement("audio");
  audio.autoplay = false;
  audio.hidden = true;
  audio.muted = !input.speakerEnabled.current;
  audio.src = url;
  document.body.append(audio);
  input.artifactAudioRef.current = audio;
  const audioContext = input.audioContextRef.current;
  input.outputMeterConnectionRef.current?.dispose();
  let meterConnection: VoiceOutputMeterConnection | null = null;
  try {
    meterConnection = audioContext ? createArtifactOutputMeter(audioContext, url, input.outputMeter, input.generation, input.generationRef) : null;
    input.outputMeterConnectionRef.current = meterConnection;
    await applyOutputDevice(audio, input.outputDeviceId.current);
    if (input.generation !== input.generationRef.current) return;
    await Promise.all([audio.play(), meterConnection?.start?.() ?? Promise.resolve()]);
    while (input.generation === input.generationRef.current && !audio.ended) {
      if (audio.error) throw new Error(audio.error.message || "Realtime audio playback failed.");
      await delay(50);
    }
  } finally {
    if (input.outputMeterConnectionRef.current === meterConnection) {
      meterConnection?.dispose();
      input.outputMeterConnectionRef.current = null;
      const currentContext = input.audioContextRef.current;
      const remoteStream = input.remoteStreamRef.current;
      if (currentContext && remoteStream && input.generation === input.generationRef.current) {
        input.outputMeterConnectionRef.current = createStreamOutputMeter(
          currentContext,
          remoteStream,
          input.outputMeter,
          input.generation,
          input.generationRef,
        );
      }
    }
    if (input.artifactAudioRef.current === audio) input.artifactAudioRef.current = null;
    audio.pause();
    audio.src = "";
    audio.remove();
    URL.revokeObjectURL(url);
  }
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, milliseconds));
}

async function waitForIceGathering(peerConnection: RTCPeerConnection, signal: AbortSignal): Promise<void> {
  if (signal.aborted) throw new Error("Home voice was cancelled.");
  if (peerConnection.iceGatheringState === "complete") return;
  await new Promise<void>((resolve, reject) => {
    const finish = (error?: Error) => {
      window.clearTimeout(timeout);
      peerConnection.removeEventListener("icegatheringstatechange", change);
      signal.removeEventListener("abort", cancel);
      if (error) reject(error); else resolve();
    };
    const cancel = () => finish(new Error("Home voice was cancelled."));
    const change = () => { if (peerConnection.iceGatheringState === "complete") finish(); };
    const timeout = window.setTimeout(() => finish(new Error("Realtime WebRTC ICE gathering timed out.")), 10_000);
    peerConnection.addEventListener("icegatheringstatechange", change);
    signal.addEventListener("abort", cancel, { once: true });
    if (signal.aborted) cancel(); else change();
  });
}

async function waitForPeerConnectionConnected(
  peerConnection: RTCPeerConnection,
  generation: number,
  generationRef: { current: number },
  timeoutMs: number,
  signal: AbortSignal,
): Promise<void> {
  if (signal.aborted || generation !== generationRef.current) throw new Error("Home voice was cancelled.");
  if (peerConnection.connectionState === "connected") return;
  await new Promise<void>((resolve, reject) => {
    const finish = (error?: Error) => {
      window.clearTimeout(timeout);
      peerConnection.removeEventListener("connectionstatechange", change);
      signal.removeEventListener("abort", cancel);
      if (error) reject(error);
      else resolve();
    };
    const cancel = () => finish(new Error("Home voice was cancelled."));
    const change = () => {
      if (signal.aborted || generation !== generationRef.current) {
        finish(new Error("Home voice was cancelled."));
        return;
      }
      const state = peerConnection.connectionState;
      if (state === "connected") {
        finish();
        return;
      }
      if (["disconnected", "failed", "closed"].includes(state)) {
        finish(new Error(`Realtime connection ${state}.`));
      }
    };
    const timeout = window.setTimeout(
      () => finish(new Error("Realtime connection timed out.")),
      timeoutMs,
    );
    peerConnection.addEventListener("connectionstatechange", change);
    signal.addEventListener("abort", cancel, { once: true });
    change();
  });
}

async function applyOutputDevice(audio: HTMLAudioElement, deviceId: string): Promise<void> {
  const element = audio as HTMLAudioElement & { setSinkId?: (sinkId: string) => Promise<void> };
  if (!deviceId || !element.setSinkId) return;
  await element.setSinkId(deviceId);
}

function cryptoRandomId(): string {
  if (typeof crypto.randomUUID !== "function") throw new Error("Secure correlation IDs are unavailable in this client.");
  return crypto.randomUUID();
}

function projectionText(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function requiredSessionDescription(value: unknown, message: string): string {
  if (typeof value !== "string" || !value.trim()) throw new Error(message);
  return value;
}

function waveformFromSamples(samples: Uint8Array): { bars: number[]; level: number } {
  let squareSum = 0;
  for (const sample of samples) {
    const normalized = (sample - 128) / 128;
    squareSum += normalized * normalized;
  }
  const rms = Math.sqrt(squareSum / samples.length);
  const level = Math.min(1, Math.max(0, rms * 4.8));
  const stride = samples.length / BAR_COUNT;
  const bars = Array.from({ length: BAR_COUNT }, (_, index) => {
    const start = Math.floor(index * stride);
    const end = Math.max(start + 1, Math.floor((index + 1) * stride));
    let peak = 0;
    for (let sampleIndex = start; sampleIndex < end; sampleIndex += 1) {
      peak = Math.max(peak, Math.abs(((samples[sampleIndex] ?? 128) - 128) / 128));
    }
    return Math.min(1, Math.max(0.035, peak * 3.2));
  });
  return { bars, level };
}

function quietWaveform(): number[] {
  return Array.from({ length: BAR_COUNT }, () => 0.035);
}

function quietMeter(): VoiceMeterSnapshot {
  return { level: 0, waveform: quietWaveform() };
}

function createVoiceMeterStore() {
  let snapshot = quietMeter();
  const listeners = new Set<() => void>();
  return {
    getSnapshot: () => snapshot,
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    set: (next: VoiceMeterSnapshot) => {
      snapshot = next;
      for (const listener of listeners) listener();
    },
  };
}

function createStreamOutputMeter(
  audioContext: AudioContext,
  stream: MediaStream,
  meter: ReturnType<typeof createVoiceMeterStore>,
  generation: number,
  generationRef: { current: number },
): VoiceOutputMeterConnection {
  const source = audioContext.createMediaStreamSource(stream);
  return createOutputMeterConnection(audioContext, source, meter, generation, generationRef, "remote");
}

function createArtifactOutputMeter(
  audioContext: AudioContext,
  url: string,
  meter: ReturnType<typeof createVoiceMeterStore>,
  generation: number,
  generationRef: { current: number },
): VoiceOutputMeterConnection {
  const element = document.createElement("audio");
  element.hidden = true;
  element.preload = "auto";
  element.src = url;
  document.body.append(element);
  let source: MediaElementAudioSourceNode | null = null;
  let connection: VoiceOutputMeterConnection;
  try {
    source = audioContext.createMediaElementSource(element);
    connection = createOutputMeterConnection(audioContext, source, meter, generation, generationRef, "artifact");
  } catch (failure) {
    source?.disconnect();
    element.src = "";
    element.remove();
    throw failure;
  }
  return {
    ...connection,
    async start() {
      await element.play();
    },
    dispose() {
      connection.dispose();
      element.pause();
      element.src = "";
      element.remove();
    },
  };
}

function createOutputMeterConnection(
  audioContext: AudioContext,
  source: AudioNode,
  meter: ReturnType<typeof createVoiceMeterStore>,
  generation: number,
  generationRef: { current: number },
  kind: VoiceOutputMeterConnection["kind"],
): VoiceOutputMeterConnection {
  const analyser = audioContext.createAnalyser();
  analyser.fftSize = 512;
  analyser.smoothingTimeConstant = 0.66;
  const sink = audioContext.createGain();
  sink.gain.value = 0;
  source.connect(analyser);
  analyser.connect(sink);
  sink.connect(audioContext.destination);
  const samples = new Uint8Array(analyser.fftSize);
  let disposed = false;
  let frame = 0;
  let lastFrameAt = 0;
  const draw = (now: number) => {
    if (disposed || generation !== generationRef.current) return;
    frame = window.requestAnimationFrame(draw);
    if (now - lastFrameAt < FRAME_INTERVAL_MS) return;
    lastFrameAt = now;
    analyser.getByteTimeDomainData(samples);
    const next = waveformFromSamples(samples);
    meter.set({ level: next.level, waveform: next.bars });
  };
  frame = window.requestAnimationFrame(draw);
  return {
    kind,
    dispose() {
      if (disposed) return;
      disposed = true;
      window.cancelAnimationFrame(frame);
      source.disconnect();
      analyser.disconnect();
      sink.disconnect();
      meter.set(quietMeter());
    },
  };
}

function recordVoiceStartup(call: VoiceCall, stage: string) {
  void window.octopusTransport?.logClientEvent?.({
    level: "info", event: "renderer.voice_startup", message: `${call.correlationId}: ${stage}`,
  }).catch(() => undefined);
}

async function requestMicrophone(deviceId: string): Promise<MediaStream> {
  return navigator.mediaDevices.getUserMedia({
    audio: {
      autoGainControl: true,
      channelCount: 1,
      deviceId: deviceId ? { exact: deviceId } : undefined,
      echoCancellation: true,
      noiseSuppression: true,
    },
  });
}

function normalizeDevices(devices: MediaDeviceInfo[], fallbackLabel: string): VoiceDeviceOption[] {
  return devices.map((device, index) => ({
    deviceId: device.deviceId,
    label: device.label || `${fallbackLabel} ${index + 1}`,
  }));
}

function chooseDeviceId(current: string, devices: VoiceDeviceOption[]): string {
  if (devices.some((device) => device.deviceId === current)) return current;
  return devices.find((device) => device.deviceId === "default")?.deviceId || devices[0]?.deviceId || "";
}

function errorText(error: unknown): string {
  return error instanceof Error && error.message ? error.message : String(error);
}

function microphoneError(error: unknown): string {
  if (error instanceof MicrophonePermissionError) {
    return `Microphone access is ${error.status}. Allow Octopus in System Settings → Privacy & Security → Microphone.`;
  }
  const name = error instanceof DOMException ? error.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") return "Allow microphone access to start voice";
  if (name === "NotFoundError" || name === "DevicesNotFoundError") return "Connect a microphone to start voice";
  if (name === "NotReadableError" || name === "TrackStartError") return "The microphone is being used by another app";
  return error instanceof Error && error.message ? error.message : "Voice input could not start";
}

class MicrophonePermissionError extends Error {
  constructor(readonly status: string) {
    super(`Microphone access is ${status}`);
  }
}
