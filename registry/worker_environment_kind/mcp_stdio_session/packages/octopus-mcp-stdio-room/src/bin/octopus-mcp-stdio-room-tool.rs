#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.as_slice() {
        [] => octopus_mcp_stdio_room::run_mcp_stdio_room_tool_process().await,
        [mode] if mode == "--example-server" => {
            octopus_mcp_stdio_room::example_server::run_example_server().await
        }
        _ => Err(format!("unsupported arguments: {args:?}")),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
