use anyhow::Result;
use clap::Parser;
use octopus_execpolicy::ExecPolicyCheckCommand;

#[doc = " CLI for evaluating exec policies"]
#[derive(Parser)]
#[command(name = "octopus-execpolicy")]
enum Cli {
    #[doc = " Evaluate a command against a policy."]
    Check(ExecPolicyCheckCommand),
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli {
        Cli::Check(cmd) => cmd.run(),
    }
}
