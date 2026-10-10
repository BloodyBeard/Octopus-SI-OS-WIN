#[tokio::main]
async fn main() {
    if let Err(error) = octopus_web_search_env::run_web_search_tool_process().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
