#[tokio::main]
async fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let result = match args.as_slice() {
        [] => octopus_system_gui_env::backend::run_desktop_host().await,
        [mode, root] if mode == "--application-settings" => application_settings(std::path::Path::new(root)),
        _ => Err("unsupported desktop host arguments".into()),
    };
    if let Err(error) = result {
        eprintln!("octopus-desktop-host: {error}");
        std::process::exit(1);
    }
}

fn application_settings(root: &std::path::Path) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut bytes = Vec::new();
    std::io::stdin().take(8193).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() > 8192 { return Err("application Settings request exceeds its bound".into()); }
    let action = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let result = octopus_system_gui_env::application_settings(root, action)?;
    writeln!(std::io::stdout().lock(), "{}", serde_json::to_string(&result).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}
