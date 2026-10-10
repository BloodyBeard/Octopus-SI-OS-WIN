#[tokio::main]
async fn main() {
    if let Err(error) = octopus_remote_exec_room::run_remote_exec_room_tool_process().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
