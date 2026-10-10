#[tokio::main]
async fn main() {
    if let Err(error) = octopus_system_coder_env::run_coder_fs_tool_process().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
