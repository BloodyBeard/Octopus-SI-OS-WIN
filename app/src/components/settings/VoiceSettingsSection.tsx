import { settingsTargetProps } from "./settingsCatalog";
import { useEffect, useMemo, useState } from "react";
import { useClientPreferences, useSetClientPreferences, type DictationProvider } from "../../clientPreferences";
import { OcButton, OcCard, OcGroup, OcSelect, OcSettingsRow, OcStatus, OcSwitch, OcTextField } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";
import type { OctopusClientAccessProfile } from "../../electron";
import { appToast, useAppErrorToast } from "../../toasts";
import { SettingsHeading } from "./SettingsHeading";
import { formatKeyboardShortcuts, useKeyboardShortcuts } from "../../keyboardShortcuts";
import { DictationRecordings } from "../chat/DictationRecordings";

export function VoiceAudioSection({ accessProfile, recordingId, onShowAllRecordings }: { accessProfile: OctopusClientAccessProfile; recordingId?: string; onShowAllRecordings?: () => void }) {
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  const keyboardShortcuts = useKeyboardShortcuts(accessProfile);
  const [audioInputs, setAudioInputs] = useState<Array<{ deviceId: string; label: string }>>([]);
  const [audioOutputs, setAudioOutputs] = useState<Array<{ deviceId: string; label: string }>>([]);
  const [deviceError, setDeviceError] = useState("");
  const [deviceLoading, setDeviceLoading] = useState(true);
  const [dictionaryExpanded, setDictionaryExpanded] = useState(false);
  const [dictionaryDraft, setDictionaryDraft] = useState<{ id: string; phrase: string; replacement: string } | null>(null);
  useAppErrorToast(deviceError);

  useEffect(() => {
    let cancelled = false;
    const mediaDevices = navigator.mediaDevices;
    const refresh = async () => {
      setDeviceLoading(true);
      if (accessProfile === "lab") {
        if (!cancelled) {
          setAudioInputs([{ deviceId: "test-microphone", label: "Test microphone" }]);
          setAudioOutputs([{ deviceId: "test-speaker", label: "Test speaker" }]);
          setDeviceError("");
          setDeviceLoading(false);
        }
        return;
      }
      if (!mediaDevices?.enumerateDevices) {
        if (!cancelled) {
          setDeviceError("Audio device enumeration is unavailable in this client host.");
          setDeviceLoading(false);
        }
        return;
      }
      try {
        const devices = await mediaDevices.enumerateDevices();
        if (cancelled) return;
        setAudioInputs(settingsAudioDevices(devices.filter((device) => device.kind === "audioinput"), "Microphone"));
        setAudioOutputs(settingsAudioDevices(devices.filter((device) => device.kind === "audiooutput"), "Speaker"));
        setDeviceError("");
      } catch (failure) {
        if (!cancelled) setDeviceError(failure instanceof Error ? failure.message : String(failure));
      } finally {
        if (!cancelled) setDeviceLoading(false);
      }
    };
    void refresh();
    const onDeviceChange = () => void refresh();
    mediaDevices?.addEventListener?.("devicechange", onDeviceChange);
    return () => {
      cancelled = true;
      mediaDevices?.removeEventListener?.("devicechange", onDeviceChange);
    };
  }, [accessProfile]);

  const inputOptions = useMemo(
    () => settingsAudioOptions(audioInputs, preferences.audioInputDeviceId, "System default"),
    [audioInputs, preferences.audioInputDeviceId],
  );
  const outputOptions = useMemo(
    () => settingsAudioOptions(audioOutputs, preferences.audioOutputDeviceId, "System default"),
    [audioOutputs, preferences.audioOutputDeviceId],
  );

  function beginDictionaryEntry(id = "") {
    const existing = preferences.dictationDictionary.find((entry) => entry.id === id);
    setDictionaryExpanded(true);
    setDictionaryDraft(existing
      ? { ...existing }
      : { id: "", phrase: "", replacement: "" });
  }

  function saveDictionaryEntry() {
    if (!dictionaryDraft) return;
    const phrase = dictionaryDraft.phrase.trim();
    const replacement = dictionaryDraft.replacement.trim();
    if (!phrase || !replacement) return;
    const duplicate = preferences.dictationDictionary.some((entry) => (
      entry.id !== dictionaryDraft.id && entry.phrase.toLocaleLowerCase() === phrase.toLocaleLowerCase()
    ));
    if (duplicate) return;
    const id = dictionaryDraft.id || `dict_${Date.now().toString(36)}_${crypto.getRandomValues(new Uint32Array(1))[0]?.toString(36)}`;
    const next = preferences.dictationDictionary.some((entry) => entry.id === id)
      ? preferences.dictationDictionary.map((entry) => entry.id === id ? { id, phrase, replacement } : entry)
      : [...preferences.dictationDictionary, { id, phrase, replacement }];
    setClientPreferences({ dictationDictionary: next });
    setDictionaryDraft(null);
  }

  const dictionaryDraftDuplicate = Boolean(dictionaryDraft?.phrase.trim()) && preferences.dictationDictionary.some((entry) => (
    entry.id !== dictionaryDraft?.id
      && entry.phrase.toLocaleLowerCase() === dictionaryDraft?.phrase.trim().toLocaleLowerCase()
  ));

  return (
    <section className="settings-section">
      <SettingsHeading
        description="Microphone, dictation, and how calls behave across the app."
        title="Voice"
      />

      <OcGroup title="Devices" description="Capture and playback devices used by voice surfaces.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("voice.microphone")}
            control={(
              <OcSelect<string>
                disabled={deviceLoading || inputOptions.length === 0}
                label="Microphone"
                onChange={(audioInputDeviceId) => setClientPreferences({ audioInputDeviceId })}
                options={inputOptions}
                value={preferences.audioInputDeviceId}
              />
            )}
            description="Used for dictation and Voice."
            label="Microphone"
          />
          <OcSettingsRow {...settingsTargetProps("voice.speaker")}
            control={(
              <OcSelect<string>
                disabled={deviceLoading || outputOptions.length === 0}
                label="Speaker"
                onChange={(audioOutputDeviceId) => setClientPreferences({ audioOutputDeviceId })}
                options={outputOptions}
                value={preferences.audioOutputDeviceId}
              />
            )}
            description="Playback device for voice replies and audio artifacts."
            label="Speaker"
          />
          <OcSettingsRow {...settingsTargetProps("voice.agent-voice")}
            control={<OcStatus label="Not declared" tone="muted" />}
            description="A selectable synthesized voice will appear only when the speech interface descriptor declares supported voices."
            label="Agent voice"
          />
        </OcCard>
      </OcGroup>

      <OcGroup title="Dictation">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("voice.transcription-provider")}
            control={(
              <OcSelect<DictationProvider>
                label="Provider"
                onChange={(dictationProvider) => setClientPreferences({ dictationProvider })}
                options={[{ value: "codex", label: "Codex" }]}
                value={preferences.dictationProvider}
              />
            )}
            description="Uses the ChatGPT session already authorized in the local Codex app. Octopus stores no separate speech API key."
            label="Transcription provider"
          />
          <OcSettingsRow {...settingsTargetProps("voice.toggle-dictation")}
            control={<ShortcutKeys keys={formatKeyboardShortcuts(keyboardShortcuts.start_dictation ?? []) || "Unassigned"} />}
            description="Start or stop dictation in the active composer."
            label="Toggle dictation"
          />
          <OcSettingsRow {...settingsTargetProps("voice.global-dictation")}
            control={(
              <OcSwitch
                checked={preferences.globalQuickDictation}
                disabled={(keyboardShortcuts.start_dictation ?? []).length === 0}
                label="Global quick dictation"
                onCheckedChange={(globalQuickDictation) => setClientPreferences({ globalQuickDictation })}
              />
            )}
            description="Bring Octopus forward, open the active task, and start the existing composer dictation from anywhere."
            label="Global quick dictation"
            meta={(keyboardShortcuts.start_dictation ?? []).length ? "Only active while this profile is unlocked." : "Assign Toggle dictation first."}
          />
          <OcSettingsRow {...settingsTargetProps("voice.hold-dictate")}
            control={<OcStatus label="Composer mic" tone="success" />}
            description="Press and hold the microphone button to record, then release to insert the transcript. Desktop-wide hold remains unavailable without a native key-up hook."
            label="Hold to dictate"
          />
          <OcSettingsRow {...settingsTargetProps("voice.dictionary")}
            control={(
              <div className="settings-inline-actions">
                {preferences.dictationDictionary.length ? (
                  <OcStatus label={`${preferences.dictationDictionary.length} ${preferences.dictationDictionary.length === 1 ? "entry" : "entries"}`} tone="neutral" />
                ) : null}
                <OcButton onClick={() => {
                  if (preferences.dictationDictionary.length && !dictionaryExpanded) {
                    setDictionaryExpanded(true);
                    return;
                  }
                  beginDictionaryEntry();
                }}>
                  {preferences.dictationDictionary.length && !dictionaryExpanded ? "Manage" : "Add entry"}
                </OcButton>
              </div>
            )}
            description="Replace recognized words or phrases before they enter the composer."
            label="Dictation dictionary"
          />
          {dictionaryExpanded ? (
            <div className="mx-3 mb-3 grid gap-1 rounded-xl bg-token-foreground/3 p-1.5">
              {preferences.dictationDictionary.map((entry) => (
                <div className="group flex min-w-0 items-center gap-3 rounded-lg px-2 py-2 hover:bg-token-list-hover-background" key={entry.id}>
                  <button className="min-w-0 flex-1 cursor-interaction text-left" onClick={() => beginDictionaryEntry(entry.id)} type="button">
                    <span className="block truncate text-sm text-token-foreground">{entry.replacement}</span>
                    <span className="mt-0.5 block truncate text-xs text-token-description-foreground">When dictation hears “{entry.phrase}”</span>
                  </button>
                  <OcButton onClick={() => beginDictionaryEntry(entry.id)} size="sm">Edit</OcButton>
                  <OcButton
                    onClick={() => {
                      const previous = preferences.dictationDictionary;
                      setClientPreferences({ dictationDictionary: previous.filter((candidate) => candidate.id !== entry.id) });
                      if (dictionaryDraft?.id === entry.id) setDictionaryDraft(null);
                      appToast.info(`Removed “${entry.phrase}”`, 8, {
                        action: { label: "Undo", onSelect: () => setClientPreferences({ dictationDictionary: previous }) },
                        dedupeKey: `dictation-dictionary-remove:${entry.id}`,
                      });
                    }}
                    size="sm"
                  >
                    Remove
                  </OcButton>
                </div>
              ))}
              {dictionaryDraft ? (
                <div className="grid gap-2 rounded-lg bg-token-main-surface-primary px-2.5 py-2.5">
                  <div className="grid grid-cols-2 gap-2 max-sm:grid-cols-1">
                    <OcTextField
                      autoFocus
                      label="When dictation hears"
                      maxLength={120}
                      onChange={(event) => setDictionaryDraft({ ...dictionaryDraft, phrase: event.currentTarget.value })}
                      placeholder="spoken phrase"
                      showLabel
                      value={dictionaryDraft.phrase}
                    />
                    <OcTextField
                      label="Insert"
                      maxLength={120}
                      onChange={(event) => setDictionaryDraft({ ...dictionaryDraft, replacement: event.currentTarget.value })}
                      placeholder="preferred spelling"
                      showLabel
                      value={dictionaryDraft.replacement}
                    />
                  </div>
                  <div className="flex items-center justify-between gap-2">
                    <span className={`text-xs ${dictionaryDraftDuplicate ? "text-token-error-foreground" : "text-token-description-foreground"}`}>
                      {dictionaryDraftDuplicate ? "That phrase already has an entry." : "Matching ignores capitalization and respects word boundaries."}
                    </span>
                    <div className="settings-inline-actions">
                      <OcButton onClick={() => setDictionaryDraft(null)} size="sm">Cancel</OcButton>
                      <OcButton
                        disabled={!dictionaryDraft.phrase.trim() || !dictionaryDraft.replacement.trim() || dictionaryDraftDuplicate}
                        onClick={saveDictionaryEntry}
                        size="sm"
                        variant="primary"
                      >
                        Save
                      </OcButton>
                    </div>
                  </div>
                </div>
              ) : (
                <div className="flex justify-between gap-2 px-2 py-1.5">
                  <OcButton onClick={() => beginDictionaryEntry()} size="sm">Add entry</OcButton>
                  <OcButton onClick={() => setDictionaryExpanded(false)} size="sm">Done</OcButton>
                </div>
              )}
            </div>
          ) : null}
          <div {...settingsTargetProps("voice.recordings")}>
            <DictationRecordings recordingId={recordingId} onShowAll={onShowAllRecordings} />
          </div>
        </OcCard>
      </OcGroup>

      <OcGroup title="Calls" description="How the active call behaves outside Home.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("voice.floating-call")}
            control={(
              <OcSwitch
                checked={preferences.floatingCallPuck}
                label="Floating call widget"
                onCheckedChange={(floatingCallPuck) => setClientPreferences({ floatingCallPuck })}
              />
            )}
            description="When you leave Home during a call, the call collapses into a small movable widget with the sphere, mute, and end call."
            label="Floating call widget"
          />
          <OcSettingsRow {...settingsTargetProps("voice.incoming-audio")}
            control={(
              <OcSelect<"ask" | "play" | "never">
                label="Incoming audio artifacts"
                onChange={(incomingAudioArtifactBehavior) => setClientPreferences({ incomingAudioArtifactBehavior })}
                options={[
                  { value: "ask", label: "Ask each time" },
                  { value: "play", label: "Play automatically" },
                  { value: "never", label: "Never play" },
                ]}
                value={preferences.incomingAudioArtifactBehavior}
              />
            )}
            description="What happens when an audio artifact arrives while a session is open."
            label="Incoming audio artifacts"
          />
        </OcCard>
      </OcGroup>


    </section>
  );
}

function settingsAudioDevices(devices: MediaDeviceInfo[], fallback: string): Array<{ deviceId: string; label: string }> {
  const seen = new Set<string>();
  return devices.flatMap((device, index) => {
    if (!device.deviceId || device.deviceId === "default" || seen.has(device.deviceId)) return [];
    seen.add(device.deviceId);
    return [{ deviceId: device.deviceId, label: device.label || `${fallback} ${index + 1}` }];
  });
}

function settingsAudioOptions(devices: Array<{ deviceId: string; label: string }>, selected: string, defaultLabel: string) {
  const options: Array<{ value: string; label: string; disabled?: boolean }> = [
    { value: "default", label: defaultLabel },
    ...devices.map((device) => ({ value: device.deviceId, label: device.label })),
  ];
  if (selected && !options.some((option) => option.value === selected)) {
    options.push({ value: selected, label: `${selected} · unavailable`, disabled: true });
  }
  return options;
}

function ShortcutKeys({ keys }: { keys: string }) {
  return (
    <span className="flex min-h-8 items-center gap-1 text-sm text-token-text-secondary">
      <kbd className="inline-flex !rounded-md !border-0 !bg-current/10 !px-2 !py-1 !font-sans !text-sm !leading-none !text-current !shadow-none">
        {keys}
      </kbd>
    </span>
  );
}
