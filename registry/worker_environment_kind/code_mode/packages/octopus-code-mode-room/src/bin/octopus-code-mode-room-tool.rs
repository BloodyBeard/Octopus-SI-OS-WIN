#[tokio::main]
async fn main() {
    if let Err(error) = octopus_code_mode_room::run_code_mode_room_tool_process().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
