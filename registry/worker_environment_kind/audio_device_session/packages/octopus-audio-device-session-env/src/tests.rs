use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use octopus_descriptors::{InterfaceId, ToolDescriptorId};
use octopus_tool_registry::ToolDescriptor;

use super::{
    encode_pcm16_wav, load_audio_interface_mode, parse_operation, pcm_metrics,
    validate_audio_tool_code_world, validate_capture_request, validate_playback_request,
    AudioBackend, AudioDeviceProcess, AudioFailure, AudioInterfaceMode, BufferSizeEvidence,
    CaptureArguments, CaptureOutput, DeviceDirection, DeviceEvidence, DeviceInventory,
    HostInventory, ListDevicesArguments, MicrophoneInterfaceConfig, PlaybackArguments,
    PlaybackOutput, SpeakerInterfaceConfig, StreamConfigEvidence, ValidatedCaptureRequest,
    ValidatedPlaybackRequest,
};

struct FakeBackend {
    playback_outcome: &'static str,
}

impl AudioBackend for FakeBackend {
    fn list_devices(
        &self,
        direction: DeviceDirection,
        _host_filter: Option<&str>,
        _max_devices: usize,
        _max_device_name_bytes: usize,
    ) -> Result<DeviceInventory, AudioFailure> {
        Ok(DeviceInventory {
            direction,
            default_host: "fake".to_string(),
            hosts: vec![HostInventory {
                id: "fake".to_string(),
                available: true,
                error: None,
                default_device: Some("fake-device".to_string()),
                default_device_error: None,
                devices: vec![DeviceEvidence {
                    name: "fake-device".to_string(),
                    is_default: true,
                    default_config: Some(StreamConfigEvidence {
                        channels: 1,
                        sample_rate: 16_000,
                        sample_format: "i16".to_string(),
                        buffer_size: BufferSizeEvidence::Unknown,
                    }),
                    config_error: None,
                }],
            }],
        })
    }

    fn capture(
        &self,
        _request: &ValidatedCaptureRequest,
        _config: &MicrophoneInterfaceConfig,
        _interface_var_dir: &Path,
    ) -> Result<CaptureOutput, AudioFailure> {
        let samples = vec![0_i16, 8192, -8192, 16384, -16384];
        let wav_bytes = encode_pcm16_wav(&samples, 1, 16_000)?;
        let (peak_dbfs, rms_dbfs) = pcm_metrics(&samples);
        Ok(CaptureOutput {
            wav_bytes,
            host: "fake".to_string(),
            device: "fake-device".to_string(),
            sample_rate: 16_000,
            channels: 1,
            frame_count: samples.len() as u64,
            duration_ms: 1,
            stop_reason: "duration_limit",
            peak_dbfs,
            rms_dbfs,
        })
    }

    fn play(
        &self,
        audio: Vec<u8>,
        _request: &ValidatedPlaybackRequest,
        _config: &SpeakerInterfaceConfig,
        _interface_var_dir: &Path,
    ) -> Result<PlaybackOutput, AudioFailure> {
        if !audio.starts_with(b"RIFF") {
            return Err(AudioFailure::new(
                "decode",
                "fake backend expected WAV bytes",
            ));
        }
        Ok(PlaybackOutput {
            host: "fake".to_string(),
            device: "fake-device".to_string(),
            elapsed_ms: 25,
            decoded_duration_ms: Some(25),
            outcome: self.playback_outcome,
        })
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .unwrap()
        .to_path_buf()
}

fn load_tool(id: &str) -> ToolDescriptor {
    let path = repository_root()
        .join("registry/tool")
        .join(id)
        .join("tool.json");
    let bytes = std::fs::read(path).unwrap();
    octopus_descriptors::parse_strict(&bytes).unwrap()
}

fn resolved_interface(id: &str) -> octopus_interfaces::InterfaceDescriptor {
    let path = repository_root()
        .join("registry/interface")
        .join(id)
        .join("interface.json");
    octopus_descriptors::parse_strict(&std::fs::read(path).unwrap()).unwrap()
}

fn microphone_config() -> MicrophoneInterfaceConfig {
    match load_audio_interface_mode(
        resolved_interface("system_microphone"),
        &InterfaceId::new("system_microphone"),
    )
    .unwrap()
    {
        AudioInterfaceMode::Microphone(config) => config,
        AudioInterfaceMode::Speaker(_) => panic!("microphone resolved as speaker"),
    }
}

fn speaker_config() -> SpeakerInterfaceConfig {
    match load_audio_interface_mode(
        resolved_interface("system_speaker"),
        &InterfaceId::new("system_speaker"),
    )
    .unwrap()
    {
        AudioInterfaceMode::Speaker(config) => config,
        AudioInterfaceMode::Microphone(_) => panic!("speaker resolved as microphone"),
    }
}

#[test]
fn descriptors_resolve_as_two_interfaces_over_one_driver_family() {
    let microphone = microphone_config();
    assert_eq!(microphone.device_direction, DeviceDirection::Input);
    assert_eq!(microphone.audio_label, "microphone.audio");
    let speaker = speaker_config();
    assert_eq!(speaker.device_direction, DeviceDirection::Output);
    assert!(speaker
        .accepted_formats
        .iter()
        .any(|format| format == "mp3"));
    for (tool, interface, operation) in [
        (
            "microphone.list_devices",
            "system_microphone",
            "list_input_devices",
        ),
        ("microphone.capture", "system_microphone", "capture_input"),
        (
            "speaker.list_devices",
            "system_speaker",
            "list_output_devices",
        ),
        ("speaker.play", "system_speaker", "play_output"),
    ] {
        let descriptor = load_tool(tool);
        validate_audio_tool_code_world(&descriptor).unwrap();
        assert_eq!(descriptor.interface.as_str(), interface);
        assert_eq!(
            descriptor.process.bin,
            "target/debug/octopus-audio-device-session-tool"
        );
        assert_eq!(
            descriptor.supported_worker_environment_kinds,
            vec![octopus_descriptors::WorkerEnvironmentKindId::new(
                "audio_device_session"
            )]
        );
        assert_eq!(descriptor.handler_config["operation"], operation);
    }
    let mut descriptor = load_tool("microphone.capture");
    descriptor.supported_worker_environment_kinds.clear();
    assert!(validate_audio_tool_code_world(&descriptor).is_err());
}

#[test]
fn exact_clone_selects_only_its_descriptor_operation() {
    let mut descriptor = load_tool("microphone.list_devices");
    assert_eq!(parse_operation(&descriptor).unwrap(), "list_input_devices");
    descriptor.id = ToolDescriptorId::new("microphone.custom_inventory");
    assert_eq!(parse_operation(&descriptor).unwrap(), "list_input_devices");

    descriptor.handler_config = serde_json::Value::Null;
    assert!(parse_operation(&descriptor).is_err());
    descriptor.handler_config = serde_json::json!({"operation": 1});
    assert!(parse_operation(&descriptor).is_err());
    descriptor.handler_config = serde_json::json!({"operation": "unknown"});
    assert!(parse_operation(&descriptor).is_err());
}

#[test]
fn capture_contract_bounds_duration_silence_and_wav_carrier() {
    let config = microphone_config();
    let request = validate_capture_request(
        CaptureArguments {
            host: Some(" CoreAudio ".to_string()),
            device_name: Some(" Built-in Microphone ".to_string()),
            max_duration_ms: 30_000,
            minimum_duration_ms: Some(1_000),
            stop_after_silence_ms: Some(750),
            silence_threshold_dbfs: Some(-42.0),
        },
        &config,
    )
    .unwrap();
    assert_eq!(request.host.as_deref(), Some("CoreAudio"));
    assert_eq!(request.device_name.as_deref(), Some("Built-in Microphone"));
    assert_eq!(request.max_duration, Duration::from_secs(30));
    assert_eq!(request.minimum_duration, Duration::from_secs(1));
    assert_eq!(request.silence.unwrap().threshold_dbfs, -42.0);
    let invalid = validate_capture_request(
        CaptureArguments {
            host: None,
            device_name: None,
            max_duration_ms: 30_000,
            minimum_duration_ms: None,
            stop_after_silence_ms: None,
            silence_threshold_dbfs: Some(-42.0),
        },
        &config,
    );
    assert!(invalid
        .unwrap_err()
        .contains("requires stop_after_silence_ms"));
    let samples = vec![i16::MIN, 0, i16::MAX, 1];
    let wav = encode_pcm16_wav(&samples, 2, 48_000).unwrap();
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(&wav[12..16], b"fmt ");
    assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 2);
    assert_eq!(
        u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
        48_000
    );
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), 8);
}

#[test]
fn playback_contract_requires_declared_format_and_explicit_deadline() {
    let config = speaker_config();
    let request = validate_playback_request(
        PlaybackArguments {
            audio_artifact_id: " artifact-audio ".to_string(),
            format: "MP3".to_string(),
            host: None,
            device_name: None,
            volume: Some(0.75),
            speed: Some(1.25),
            max_duration_ms: Some(15_000),
        },
        &config,
    )
    .unwrap();
    assert_eq!(request.source_artifact_id, "artifact-audio");
    assert_eq!(request.format, "mp3");
    assert_eq!(request.max_duration, Duration::from_secs(15));
    let invalid = validate_playback_request(
        PlaybackArguments {
            audio_artifact_id: "artifact-audio".to_string(),
            format: "raw".to_string(),
            host: None,
            device_name: None,
            volume: None,
            speed: None,
            max_duration_ms: None,
        },
        &config,
    );
    assert!(invalid.unwrap_err().contains("not accepted"));
}

#[tokio::test]
async fn fake_device_boundary_materializes_audio_inventory_and_outcomes() {
    let root = tempfile::tempdir().unwrap();
    let microphone = microphone_config();
    let microphone_process = AudioDeviceProcess {
        interface: InterfaceId::new("system_microphone"),
        interface_var_dir: root.path().join("microphone-var"),
        max_manifest_bytes: 4096,
        max_tool_call_bytes: 4096,
        mode: AudioInterfaceMode::Microphone(microphone.clone()),
        backend: Arc::new(FakeBackend {
            playback_outcome: "completed",
        }),
    };
    let capture = microphone_process
        .execute_capture(
            CaptureArguments {
                host: None,
                device_name: None,
                max_duration_ms: 1_000,
                minimum_duration_ms: None,
                stop_after_silence_ms: None,
                silence_threshold_dbfs: None,
            },
            microphone,
            &load_tool("microphone.capture"),
            Some("capture-1".to_string()),
            root.path().join("capture-output"),
        )
        .await
        .unwrap_or_else(|_| panic!("fake capture execution failed"));
    assert_eq!(capture.len(), 2);
    assert_eq!(
        capture[0].read_manifest().unwrap().label,
        "microphone.audio"
    );
    assert!(capture[0]
        .read_payload_bytes()
        .unwrap()
        .starts_with(b"RIFF"));
    let capture_result: serde_json::Value =
        serde_json::from_slice(&capture[1].read_payload_bytes().unwrap()).unwrap();
    assert_eq!(capture_result["ok"], true);
    assert_eq!(capture_result["correlation_id"], "capture-1");

    let devices = microphone_process
        .execute_list_devices(
            DeviceDirection::Input,
            ListDevicesArguments { host: None },
            128,
            1024,
            "microphone.devices",
            &load_tool("microphone.list_devices"),
            None,
            root.path().join("devices-output"),
        )
        .await
        .unwrap_or_else(|_| panic!("fake device inventory execution failed"));
    assert_eq!(devices.len(), 2);
    let inventory: serde_json::Value =
        serde_json::from_slice(&devices[0].read_payload_bytes().unwrap()).unwrap();
    assert_eq!(inventory["hosts"][0]["devices"][0]["name"], "fake-device");
    assert_eq!(
        devices[1].read_manifest().unwrap().label,
        "microphone.list_devices.result"
    );

    let speaker = speaker_config();
    let speaker_process = AudioDeviceProcess {
        interface: InterfaceId::new("system_speaker"),
        interface_var_dir: root.path().join("speaker-var"),
        max_manifest_bytes: 4096,
        max_tool_call_bytes: 4096,
        mode: AudioInterfaceMode::Speaker(speaker.clone()),
        backend: Arc::new(FakeBackend {
            playback_outcome: "stopped_at_duration_limit",
        }),
    };
    let playback = speaker_process
        .execute_playback(
            PlaybackArguments {
                audio_artifact_id: "artifact-audio".to_string(),
                format: "wav".to_string(),
                host: None,
                device_name: None,
                volume: None,
                speed: None,
                max_duration_ms: Some(1_000),
            },
            capture[0].read_payload_bytes().unwrap(),
            speaker,
            &load_tool("speaker.play"),
            Some("playback-1".to_string()),
            root.path().join("playback-output"),
        )
        .await
        .unwrap_or_else(|_| panic!("fake playback execution failed"));
    assert_eq!(playback.len(), 1);
    let playback_result: serde_json::Value =
        serde_json::from_slice(&playback[0].read_payload_bytes().unwrap()).unwrap();
    assert_eq!(playback_result["ok"], false);
    assert_eq!(
        playback_result["error"]["code"],
        "audio_playback_incomplete"
    );
    assert_eq!(
        playback_result["error"]["details"]["operation_body"]["outcome"],
        "stopped_at_duration_limit"
    );
    assert_eq!(playback_result["correlation_id"], "playback-1");
}
