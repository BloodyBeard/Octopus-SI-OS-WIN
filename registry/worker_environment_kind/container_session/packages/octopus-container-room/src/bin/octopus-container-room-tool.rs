#[tokio::main]
async fn main() {
    if let Err(error) = octopus_container_room::run_container_room_tool_process().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
