use clap::Parser;
use octopus_tui::AppExitInfo;
use octopus_tui::Cli;
use octopus_tui::ExitReason;
use octopus_tui::TerminalRuntimePaths;
use octopus_tui::run_main;
use octopus_utils_cli::CliConfigOverrides;
use std::io::Write;
use supports_color::Stream;

const TOKIO_WORKER_STACK_SIZE_BYTES: usize = 16 * 1024 * 1024;

fn format_exit_messages(exit_info: AppExitInfo, color_enabled: bool) -> Vec<String> {
    let is_fatal = matches!(&exit_info.exit_reason, ExitReason::Fatal(_));
    let AppExitInfo {
        token_usage,
        thread_id,
        resume_hint,
        ..
    } = exit_info;

    let mut lines = Vec::new();
    if !token_usage.is_zero() {
        lines.push(token_usage.to_string());
    }

    if let Some(resume_cmd) = resume_hint {
        let command = if color_enabled {
            format!("\u{1b}[36m{resume_cmd}\u{1b}[39m")
        } else {
            resume_cmd
        };
        lines.push(format!("To continue this chat, run {command}"));
    } else if is_fatal && let Some(run_id) = thread_id {
        lines.push(format!("Run ID: {run_id}"));
    }

    lines
}

#[derive(Parser, Debug)]
struct TopCli {
    #[clap(flatten)]
    config_overrides: CliConfigOverrides,

    #[clap(flatten)]
    inner: Cli,
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os();
    let _ = args.next();
    if args.next().as_deref() == Some(std::ffi::OsStr::new(octopus_exec::CODEX_FS_HELPER_ARG1)) {
        octopus_exec::run_fs_helper_main();
    }

    let runtime_paths = TerminalRuntimePaths {
        self_exe: std::env::current_exe().ok(),
        linux_sandbox_exe: None,
        execve_wrapper_exe: None,
    };
    let handle = std::thread::Builder::new()
        .name("octopus-terminal-main".to_string())
        .stack_size(TOKIO_WORKER_STACK_SIZE_BYTES)
        .spawn(move || {
            let mut runtime = tokio::runtime::Builder::new_multi_thread();
            runtime.enable_all();
            runtime.thread_stack_size(TOKIO_WORKER_STACK_SIZE_BYTES);
            runtime.build()?.block_on(async move {
                let top_cli = TopCli::parse();
                let mut inner = top_cli.inner;
                inner
                    .config_overrides
                    .raw_overrides
                    .splice(0..0, top_cli.config_overrides.raw_overrides);
                let exit_info = run_main(
                    inner,
                    runtime_paths,
                    None,
                    octopus_tui::LauncherProviderHooks::default(),
                )
                .await?;
                let is_fatal = match &exit_info.exit_reason {
                    ExitReason::Fatal(message) => {
                        eprintln!("ERROR: {message}");
                        true
                    }
                    ExitReason::UserRequested => false,
                };

                let color_enabled = supports_color::on(Stream::Stdout).is_some();
                for line in format_exit_messages(exit_info, color_enabled) {
                    println!("{line}");
                }
                if is_fatal {
                    std::io::stdout().flush()?;
                    std::process::exit(1);
                }
                Ok(())
            })
        })?;
    match handle.join() {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}
