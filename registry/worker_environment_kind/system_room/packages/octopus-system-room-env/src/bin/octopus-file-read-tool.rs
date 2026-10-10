#[tokio::main]
async fn main() {
    if let Err(error) = octopus_system_room_env::run_file_read_tool_process().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
