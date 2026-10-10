#[tokio::main]
async fn main() {
    if let Err(error) = octopus_system_exec_env::run_process_room_tool_process().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
