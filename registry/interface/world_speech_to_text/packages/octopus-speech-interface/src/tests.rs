use reqwest::header::HeaderMap;
use reqwest::Client;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{
    SpeechDefaults, SpeechMode, SpeechProcess, SpeechTransport, SynthesisArguments,
    SynthesisOperation, TranscriptionArguments, TranscriptionOperation,
};

struct CapturedRequest {
    head: String,
    body: Vec<u8>,
}

async fn spawn_response(
    status: u16,
    content_type: &str,
    body: Vec<u8>,
    path: &str,
) -> (reqwest::Url, tokio::task::JoinHandle<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = reqwest::Url::parse(&format!("http://{address}{path}")).unwrap();
    let content_type = content_type.to_string();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        let (head_end, content_length) = loop {
            let read = socket.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&chunk[..read]);
            if let Some(head_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let head_end = head_end + 4;
                let head = String::from_utf8_lossy(&request[..head_end]);
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                break (head_end, content_length);
            }
        };
        while request.len() - head_end < content_length {
            let read = socket.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&chunk[..read]);
        }
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
        socket.flush().await.unwrap();
        CapturedRequest {
            head: String::from_utf8_lossy(&request[..head_end]).to_string(),
            body: request[head_end..head_end + content_length].to_vec(),
        }
    });
    (endpoint, handle)
}

fn process(endpoint: reqwest::Url, mode: SpeechMode, max_response_bytes: usize) -> SpeechProcess {
    SpeechProcess {
        transport: SpeechTransport {
            endpoint,
            headers: HeaderMap::new(),
            auth: None,
            max_response_bytes,
        },
        defaults: match mode {
            SpeechMode::Transcribe => SpeechDefaults {
                model: "transcription-model".to_string(),
                voice: None,
                format: None,
            },
            SpeechMode::Synthesize => SpeechDefaults {
                model: "speech-model".to_string(),
                voice: Some("alloy".to_string()),
                format: Some("mp3".to_string()),
            },
        },
        max_manifest_bytes: 4096,
        client: Client::new(),
    }
}

fn transcription_operation() -> TranscriptionOperation {
    TranscriptionOperation {
        file_field: "file".to_string(),
        model_field: "model".to_string(),
        language_field: "language".to_string(),
        prompt_field: "prompt".to_string(),
        response_format_field: "response_format".to_string(),
        response_format: "json".to_string(),
        transcript_path: "text".to_string(),
        language_path: "language".to_string(),
        duration_path: "duration".to_string(),
        transcript_label: "speech.transcript".to_string(),
        max_audio_bytes: Some(4096),
        max_transcript_bytes: Some(4096),
    }
}

fn synthesis_operation() -> SynthesisOperation {
    SynthesisOperation {
        text_field: "input".to_string(),
        model_field: "model".to_string(),
        voice_field: "voice".to_string(),
        format_field: "response_format".to_string(),
        speed_field: "speed".to_string(),
        audio_label: "speech.audio".to_string(),
        expected_content_types: vec!["audio/mpeg".to_string()],
        max_text_bytes: Some(4096),
        max_audio_bytes: Some(4096),
    }
}

#[tokio::test]
async fn transcription_sends_bound_audio_and_emits_typed_fields() {
    let response = serde_json::to_vec(&serde_json::json!({
        "text": "hello octopus",
        "language": "en",
        "duration": 1.25
    }))
    .unwrap();
    let (endpoint, request) = spawn_response(200, "application/json", response, "/audio").await;
    let output = process(endpoint, SpeechMode::Transcribe, 4096)
        .transcribe(
            &transcription_operation(),
            &TranscriptionArguments {
                audio_artifact_id: "artifact-audio".to_string(),
                filename: "voice.wav".to_string(),
                media_type: Some("audio/wav".to_string()),
                language: Some("en".to_string()),
                prompt: Some("Octopus".to_string()),
            },
            b"RIFF-audio-evidence".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(output.text, "hello octopus");
    assert_eq!(output.language.as_deref(), Some("en"));
    assert_eq!(output.duration, Some(1.25));
    let request = request.await.unwrap();
    assert!(request.head.starts_with("POST /audio HTTP/1.1"));
    assert!(request
        .head
        .to_ascii_lowercase()
        .contains("multipart/form-data"));
    let body = String::from_utf8_lossy(&request.body);
    assert!(body.contains("RIFF-audio-evidence"));
    assert!(body.contains("name=\"model\""));
    assert!(body.contains("transcription-model"));
    assert!(body.contains("name=\"language\""));
    assert!(body.contains("name=\"prompt\""));
}

#[tokio::test]
async fn synthesis_sends_json_and_preserves_bounded_audio_bytes() {
    let audio = b"ID3-audio-evidence".to_vec();
    let (endpoint, request) = spawn_response(200, "audio/mpeg", audio.clone(), "/speech").await;
    let output = process(endpoint, SpeechMode::Synthesize, 4096)
        .synthesize(
            &synthesis_operation(),
            &SynthesisArguments {
                text: "Speak clearly".to_string(),
                voice: None,
                format: None,
                speed: Some(1.1),
            },
        )
        .await
        .unwrap();
    assert_eq!(output.bytes, audio);
    assert_eq!(output.content_type, "audio/mpeg");
    let request = request.await.unwrap();
    assert!(request.head.starts_with("POST /speech HTTP/1.1"));
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["input"], "Speak clearly");
    assert_eq!(body["model"], "speech-model");
    assert_eq!(body["voice"], "alloy");
    assert_eq!(body["response_format"], "mp3");
    assert_eq!(body["speed"], 1.1);
}

#[test]
fn tracked_headers_cannot_embed_interface_secrets() {
    let name = super::parse_header_name("Authorization", "test").unwrap();
    assert!(super::reject_secret_header(&name).is_err());
    assert!(super::parse_http_url("http://speech.example/v1", "test").is_err());
    assert!(super::parse_http_url("http://localhost:8080/v1", "test").is_ok());
    assert!(super::parse_http_url("https://speech.example/v1", "test").is_ok());
}

#[test]
fn concrete_speech_interfaces_own_their_transform_contracts() {
    let package = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let transcription: octopus_interfaces::InterfaceDescriptor =
        serde_json::from_slice(&std::fs::read(package.join("../../interface.json")).unwrap())
            .unwrap();
    let transcription_config = transcription.interface_driver_config.as_ref().unwrap();
    let transcription_operation =
        super::parse_transcription_operation(transcription_config, &transcription.id).unwrap();
    assert_eq!(
        transcription_operation.transcript_label,
        "speech.transcript"
    );
    assert_eq!(
        super::metadata_string(
            transcription_config,
            &transcription.id,
            "input_request_label"
        )
        .unwrap(),
        "speech.transcription.request"
    );

    let synthesis: octopus_interfaces::InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(package.join("../../../world_text_to_speech/interface.json")).unwrap(),
    )
    .unwrap();
    let synthesis_config = synthesis.interface_driver_config.as_ref().unwrap();
    let synthesis_operation =
        super::parse_synthesis_operation(synthesis_config, &synthesis.id).unwrap();
    assert_eq!(synthesis_operation.audio_label, "speech.audio");
    assert_eq!(
        super::metadata_string(synthesis_config, &synthesis.id, "input_request_label").unwrap(),
        "speech.synthesis.request"
    );
}
