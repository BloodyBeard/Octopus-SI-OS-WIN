use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    about = "Generate TypeScript bindings and JSON Schemas for the Codex app-server protocol"
)]
struct Args {
    #[doc = " Output directory where generated files will be written"]
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    #[doc = " Optional Prettier executable path to format generated TypeScript files"]
    #[arg(short = 'p', long = "prettier", value_name = "PRETTIER_BIN")]
    prettier: Option<PathBuf>,

    #[doc = " Include experimental API methods and fields in generated output."]
    #[arg(long = "experimental")]
    experimental: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    octopus_appserver_proto::generate_ts_with_options(
        &args.out_dir,
        args.prettier.as_deref(),
        octopus_appserver_proto::GenerateTsOptions {
            experimental_api: args.experimental,
            ..octopus_appserver_proto::GenerateTsOptions::default()
        },
    )?;
    octopus_appserver_proto::generate_json_with_experimental(&args.out_dir, args.experimental)
}
