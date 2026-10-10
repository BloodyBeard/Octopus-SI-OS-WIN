#[tokio::main]
async fn main() {
    if let Err(error) = octopus_system_room_env::run_artifact_bound_tool_process().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
