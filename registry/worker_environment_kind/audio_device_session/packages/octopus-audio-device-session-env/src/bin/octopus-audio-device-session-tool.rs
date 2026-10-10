#[tokio::main]
async fn main() {
    if let Err(error) = octopus_audio_device_session_env::run_audio_device_tool_process().await {
        eprintln!("octopus-audio-device-session-tool failed: {error}");
        std::process::exit(1);
    }
}
