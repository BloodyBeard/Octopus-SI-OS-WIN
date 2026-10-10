#[tokio::main]
async fn main() {
    if let Err(error) = octopus_browser_session_env::run_browser_session_tool_process().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
