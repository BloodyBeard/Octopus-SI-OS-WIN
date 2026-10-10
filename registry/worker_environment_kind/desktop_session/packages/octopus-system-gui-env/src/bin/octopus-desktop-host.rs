#[tokio::main]
async fn main() {
    if let Err(error) = octopus_system_gui_env::backend::run_desktop_host().await {
        eprintln!("octopus-desktop-host: {error}");
        std::process::exit(1);
    }
}
