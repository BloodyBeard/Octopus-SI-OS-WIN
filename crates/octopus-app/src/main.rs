#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use octopus_interface_transport::{
    prepare_interface_activation_reconciler, InterfaceActivationArrival, InterfaceBootEvent,
    SystemInterfaceEmission,
};
use octopus_ledger_store::DurableBlobs;
use std::sync::Arc;

mod boot_report;
mod bootstrap_ees;
mod daemon;
mod doctor;
mod ee_state_migration;
mod ledger_writer;
mod registry_install;
mod runtime_instance;
pub use daemon::run_daemon;

fn installed_source_registry_root(
    installation_root: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = std::env::var_os("OCTOPUS_SOURCE_REGISTRY")
        .map(PathBuf::from)
        .unwrap_or_else(|| installation_root.join("registry"));
    std::fs::canonicalize(&path)
        .map_err(|error| {
            format!(
                "source registry root '{}' cannot be canonicalized: {error}",
                path.display()
            )
        })
        .map_err(Into::into)
}

fn runtime_installation_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = match std::env::var_os("OCTOPUS_INSTALLATION_ROOT") {
        Some(root) => PathBuf::from(root),
        None => {
            let executable = std::env::current_exe()?;
            let profile_root = executable
                .parent()
                .ok_or("runtime executable has no profile directory")?;
            if profile_root.file_name().and_then(|name| name.to_str()) == Some("bin") {
                profile_root
                    .parent()
                    .ok_or("runtime executable bin directory has no installation root")?
                    .to_path_buf()
            } else {
                let target_root = profile_root
                    .parent()
                    .ok_or("runtime executable has no target directory")?;
                if target_root.file_name().and_then(|name| name.to_str()) != Some("target") {
                    return Err(format!(
                        "runtime executable '{}' is outside a bin installation or target profile; OCTOPUS_INSTALLATION_ROOT is required",
                        executable.display()
                    )
                    .into());
                }
                target_root
                    .parent()
                    .ok_or("runtime executable target directory has no installation root")?
                    .to_path_buf()
            }
        }
    };
    if !root.is_absolute() {
        return Err(format!(
            "runtime installation root must be absolute: {}",
            root.display()
        )
        .into());
    }
    let root = std::fs::canonicalize(&root).map_err(|error| {
        format!(
            "runtime installation root '{}' cannot be canonicalized: {error}",
            root.display()
        )
    })?;
    if !root.join("registry").is_dir() {
        return Err(format!(
            "runtime installation root '{}' has no registry directory",
            root.display()
        )
        .into());
    }
    Ok(root)
}

fn octopus_home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("OCTOPUS_HOME") {
        return PathBuf::from(home);
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".octopus"))
        .unwrap_or_else(|| PathBuf::from(".octopus"))
}

fn spine_durable_root() -> PathBuf {
    if let Some(home) = std::env::var_os("OCTOPUS_HOME") {
        return PathBuf::from(home).join("ledger");
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".octopus"))
        .unwrap_or_else(|| PathBuf::from(".octopus"))
        .join("ledger")
}

async fn forward_interface_activation_arrivals(
    mut rx: tokio::sync::mpsc::Receiver<InterfaceActivationArrival>,
    tx: tokio::sync::mpsc::Sender<daemon::Arrival>,
) {
    while let Some(arrival) = rx.recv().await {
        if tx
            .send(daemon::Arrival {
                artifact: arrival.artifact,
                interface: arrival.interface,
                evidence: arrival.evidence,
                verdict_tx: arrival.verdict_tx,
                retained: arrival.retained,
            })
            .await
            .is_err()
        {
            break;
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    if args.next().is_some() {
        return usage();
    }
    match command.as_deref() {
        Some("up") => up().await,
        Some("down") => down(),
        Some("doctor") => doctor().await,
        Some("migrate-ee-instance-schema") => migrate_ee_instance_schema(),
        Some("project-ee-policies") => project_ee_policies(),
        _ => usage(),
    }
}

#[cfg(unix)]
async fn shutdown_signal() {
    let mut terminate =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                eprintln!("octopus: SIGTERM handler failed to register: {error}");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            let _ = result;
        }
        _ = terminate.recv() => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn usage() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("octopus");
    eprintln!("  up      boot the OS: synchronize the registry, open state, wake every interface");
    eprintln!("  down    stop the running instance cleanly");
    eprintln!("  doctor  scan the active registry and declared life probes without booting");
    eprintln!("  migrate-ee-instance-schema");
    eprintln!("  project-ee-policies");
    std::process::exit(2);
}

async fn doctor() -> Result<(), Box<dyn std::error::Error>> {
    let octopus_home = octopus_home_dir();
    let installation_root = runtime_installation_root()?;
    doctor::run(&octopus_home, &installation_root)
        .await
        .map_err(Into::into)
}

fn project_ee_policies() -> Result<(), Box<dyn std::error::Error>> {
    let octopus_home = octopus_home_dir();
    let projection = octopus_app_server::project_ee_policies(
        &octopus_home.join("registry"),
        &octopus_home.join("state").join("ee_instances"),
    )?;
    println!("{}", serde_json::to_string(&projection)?);
    Ok(())
}

fn migrate_ee_instance_schema() -> Result<(), Box<dyn std::error::Error>> {
    let octopus_home = octopus_home_dir();
    if let Some(record) = runtime_instance::observe(&octopus_home)? {
        return Err(format!(
            "octopus pid {} is running; stop it before migrating EE instance records",
            record.pid
        )
        .into());
    }
    let outcome = ee_state_migration::migrate(&octopus_home)?;
    println!(
        "EE instance schema: migrated {}, already current {}",
        outcome.migrated, outcome.current
    );
    Ok(())
}

fn down() -> Result<(), Box<dyn std::error::Error>> {
    let octopus_home = octopus_home_dir();
    let mut boot = boot_report::BootReport::new();
    boot.banner("octopus");
    boot.stage("home", octopus_home.display());
    let Some(record) = runtime_instance::observe(&octopus_home)? else {
        boot.stage("instance", "not running");
        return Ok(());
    };
    boot.stage(
        "instance",
        format_args!("pid {} · phase {}", record.pid, record.phase),
    );
    runtime_instance::interrupt(record.pid)?;
    boot.stage("signal", "SIGTERM delivered");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while runtime_instance::observe(&octopus_home)?.is_some() {
        if std::time::Instant::now() >= deadline {
            return Err(format!("octopus pid {} did not stop within 10s", record.pid).into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    boot.done("stopped", format_args!("· pid {}", record.pid));
    Ok(())
}

async fn up() -> Result<(), Box<dyn std::error::Error>> {
    let octopus_home = octopus_home_dir();
    let mut boot = boot_report::BootReport::new();
    boot.banner("octopus");
    boot.stage("home", octopus_home.display());
    let cancel = CancellationToken::new();
    let cancel_for_signal = cancel.clone();
    let _shutdown_signal_task = tokio::spawn(async move {
        shutdown_signal().await;
        eprintln!("octopus: shutdown signal received — cancelling the daemon cleanly");
        cancel_for_signal.cancel();
    });
    let mut runtime_instance = runtime_instance::RuntimeInstance::acquire(&octopus_home)?;
    if cancel.is_cancelled() {
        return Ok(());
    }
    runtime_instance.set_phase("installing_registry")?;
    let runtime_installation_root = runtime_installation_root()?;
    let source_registry_root = installed_source_registry_root(&runtime_installation_root)?;
    boot.stage(
        "registry",
        format_args!("synchronizing from {}", source_registry_root.display()),
    );
    let registry_install = registry_install::install_registry_with_progress(
        &source_registry_root,
        &octopus_home.join("registry"),
        &mut |event| match event {
            registry_install::RegistrySyncProgress::Scanned { files } => {
                boot.stage("registry", format_args!("{files} source descriptors"));
            }
            registry_install::RegistrySyncProgress::Applying {
                done,
                total,
                relative,
            } => {
                boot.progress("registry", done, total, relative);
            }
        },
    )?;
    if cancel.is_cancelled() {
        return Ok(());
    }
    let registry_root_path = registry_install.root;
    runtime_instance.set_phase("registry_ready")?;
    boot.stage(
        "registry",
        format_args!(
            "ready at {} · copied {} · updated {} · removed {} · kept local {}",
            registry_root_path.display(),
            registry_install.copied,
            registry_install.updated,
            registry_install.removed,
            registry_install.preserved_local
        ),
    );
    let durable_root = spine_durable_root();
    octopus_file_storage::publication::create_dir_all(&durable_root)?;
    let durable_root = std::fs::canonicalize(durable_root)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(durable_root.join("movements.jsonl"))?;
    let state_root = octopus_home.join("state");
    boot.stage("state", format_args!("opening {}", state_root.display()));
    let default_runs_dir = state_root.join("runs");
    let default_ee_instances_dir = state_root.join("ee_instances");
    let interface_artifact_root = state_root.join("interface-artifacts");
    let source_root = source_registry_root
        .parent()
        .ok_or("source registry root has no parent")?;
    let host_roots = daemon::ensure_host_manifest(
        &octopus_home,
        &registry_root_path,
        source_root,
        &default_runs_dir,
        &default_ee_instances_dir,
    )?;
    runtime_instance.set_phase("state_ready")?;
    let runs_dir = host_roots
        .get("run_records_root")
        .ok_or("host.json resolved no run_records_root")?
        .to_path_buf();
    octopus_file_storage::publication::create_dir_all(&runs_dir)?;
    let runs_dir = std::fs::canonicalize(runs_dir)?;
    let ee_instances_dir = host_roots
        .get("ee_instances_root")
        .ok_or("host.json resolved no ee_instances_root")?
        .to_path_buf();
    let blobs: octopus_app_server::SharedBlobs = Arc::new(std::sync::Mutex::new(
        DurableBlobs::open(durable_root.join("blobs"))?,
    ));

    let (arrivals_tx, arrivals_rx) = tokio::sync::mpsc::channel::<daemon::Arrival>(64);
    let (activation_arrivals_tx, activation_arrivals_rx) =
        tokio::sync::mpsc::channel::<InterfaceActivationArrival>(64);
    let (emissions_tx, emissions_rx) = tokio::sync::mpsc::channel::<SystemInterfaceEmission>(64);
    if cancel.is_cancelled() {
        return Ok(());
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let activation_arrival_forwarder_task = tokio::task::spawn_local(
                forward_interface_activation_arrivals(activation_arrivals_rx, arrivals_tx),
            );
            let cancel_after_daemon = cancel.clone();
            let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel();
            let (daemon_boot_tx, mut daemon_boot_rx) = tokio::sync::mpsc::unbounded_channel();
            let daemon_durable_root = durable_root.clone();
            let daemon_registry_root = registry_root_path.clone();
            let daemon_cancel = cancel.clone();
            let mut daemon_task = tokio::task::spawn_local(async move {
                daemon::run_daemon_on_dirs(
                    &daemon_durable_root,
                    daemon_registry_root,
                    interface_artifact_root,
                    runs_dir,
                    ee_instances_dir,
                    blobs,
                    arrivals_rx,
                    emissions_rx,
                    daemon_cancel,
                    Some(ready_tx),
                    Some(daemon_boot_tx),
                )
                .await
                .map_err(|error| error.to_string())
            });
            let startup_result: Result<bool, Box<dyn std::error::Error>> = loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break Ok(false),
                    Some(event) = daemon_boot_rx.recv() => report_daemon_boot_event(&mut boot, event),
                    readiness = &mut ready_rx => break match readiness {
                        Ok(()) => Ok(true),
                        Err(_) => match (&mut daemon_task).await {
                            Ok(Ok(())) => Err("daemon stopped before completing initialization".into()),
                            Ok(Err(error)) => Err(error.into()),
                            Err(error) => Err(std::io::Error::other(format!(
                                "daemon task failed before completing initialization: {error}"
                            )).into()),
                        },
                    },
                    result = &mut daemon_task => break match result {
                        Ok(Ok(())) => Err("daemon stopped before completing initialization".into()),
                        Ok(Err(error)) => Err(error.into()),
                        Err(error) => Err(std::io::Error::other(format!(
                            "daemon task failed before completing initialization: {error}"
                        )).into()),
                    },
                }
            };
            while let Ok(event) = daemon_boot_rx.try_recv() {
                report_daemon_boot_event(&mut boot, event);
            }
            let daemon_ready = match startup_result {
                Ok(ready) => ready,
                Err(error) => {
                    cancel_after_daemon.cancel();
                    activation_arrival_forwarder_task.abort();
                    return Err(error);
                }
            };
            if !daemon_ready {
                let daemon_result = daemon_task.await;
                activation_arrival_forwarder_task.abort();
                return match daemon_result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => Err(error.into()),
                    Err(error) => Err(std::io::Error::other(format!(
                        "daemon task failed during initialization cleanup: {error}"
                    ))
                    .into()),
                };
            }

            boot.stage("interfaces", "discovering descriptor-owned life processes");
            let mut report_interface_boot = |event| match event {
                InterfaceBootEvent::Discovered { total } => {
                    boot.stage("interfaces", format_args!("lifting {total} descriptor-owned life process(es)"));
                }
                InterfaceBootEvent::Ready {
                    interface,
                    completed,
                    total,
                } => boot.progress(
                    "interfaces",
                    completed,
                    total,
                    &format!("{} ready", interface.as_str()),
                ),
                InterfaceBootEvent::Failed {
                    interface,
                    completed,
                    total,
                    ..
                } => boot.progress(
                    "interfaces",
                    completed,
                    total,
                    &format!("{} in coma", interface.as_str()),
                ),
            };
            let interface_activation_reconciler = match prepare_interface_activation_reconciler(
                registry_root_path,
                runtime_installation_root,
                activation_arrivals_tx,
                emissions_tx,
                cancel.clone(),
                octopus_home,
                Some(durable_root.clone()),
                Some(&mut report_interface_boot),
            )
            .await
            {
                Ok(reconciler) => reconciler,
                Err(error) => {
                    let shutdown_requested = cancel.is_cancelled();
                    cancel_after_daemon.cancel();
                    let daemon_result = daemon_task.await;
                    activation_arrival_forwarder_task.abort();
                    if shutdown_requested {
                        return match daemon_result {
                            Ok(Ok(())) => Ok(()),
                            Ok(Err(error)) => Err(error.into()),
                            Err(error) => Err(std::io::Error::other(format!(
                                "daemon task failed during interface cleanup: {error}"
                            ))
                            .into()),
                        };
                    }
                    return Err(error);
                }
            };
            let initial_interface_lives = interface_activation_reconciler.initial_task_count;
            let initial_interface_failures = interface_activation_reconciler.initial_failures.len();
            for (interface, error) in &interface_activation_reconciler.initial_failures {
                eprintln!(
                    "interface '{}' remains in coma after runtime initialization: {error}",
                    interface.as_str()
                );
            }
            let activation_reconciler_task =
                tokio::task::spawn_local(interface_activation_reconciler.reconciler.run());

            if cancel.is_cancelled() {
                let daemon_result = daemon_task.await;
                let activation_result = activation_reconciler_task.await;
                activation_arrival_forwarder_task.abort();
                if let Err(error) = activation_result {
                    return Err(std::io::Error::other(format!(
                        "interface lifecycle cleanup task failed: {error}"
                    ))
                    .into());
                }
                return match daemon_result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => Err(error.into()),
                    Err(error) => Err(std::io::Error::other(format!(
                        "daemon task failed during interface cleanup: {error}"
                    ))
                    .into()),
                };
            }

            if let Err(error) = runtime_instance.set_phase("ready") {
                cancel_after_daemon.cancel();
                let _ = daemon_task.await;
                let _ = activation_reconciler_task.await;
                activation_arrival_forwarder_task.abort();
                return Err(error);
            }
            boot.stage(
                "interfaces",
                format_args!(
                    "{initial_interface_lives} descriptor-owned life process(es) awake · {initial_interface_failures} in coma"
                ),
            );
            boot.done(
                "ready",
                format_args!(
                    "· durable {} · octopus down stops cleanly",
                    durable_root.display()
                ),
            );
            let result: Result<(), Box<dyn std::error::Error>> = match daemon_task.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(error.into()),
                Err(error) => {
                    Err(std::io::Error::other(format!("daemon task failed: {error}")).into())
                }
            };
            cancel_after_daemon.cancel();
            if let Err(error) = activation_reconciler_task.await {
                activation_arrival_forwarder_task.abort();
                return Err(std::io::Error::other(format!(
                    "interface lifecycle cleanup task failed: {error}"
                ))
                .into());
            }
            activation_arrival_forwarder_task.abort();
            result
        })
        .await?;
    Ok(())
}

fn report_daemon_boot_event(boot: &mut boot_report::BootReport, event: daemon::DaemonBootEvent) {
    match event {
        daemon::DaemonBootEvent::LedgerOpening { path } => {
            boot.stage("ledger", format_args!("opening {}", path.display()));
        }
        daemon::DaemonBootEvent::LedgerProjected {
            movements,
            unique_grants,
        } => boot.stage(
            "ledger",
            format_args!("{movements} movements · {unique_grants} unique grants verified"),
        ),
        daemon::DaemonBootEvent::SpineOpening => {
            boot.stage("spine", "opening embedded runtime");
        }
        daemon::DaemonBootEvent::SpineReady => {
            boot.stage("spine", "embedded runtime ready");
        }
        daemon::DaemonBootEvent::CheckpointsReconstructing => {
            boot.stage("checkpoints", "reconstructing durable admissions");
        }
        daemon::DaemonBootEvent::CheckpointsReconstructed => {
            boot.stage("checkpoints", "durable admissions reconstructed");
        }
        daemon::DaemonBootEvent::OutboundTailRecovering => {
            boot.stage("recovery", "checking durable outbound tail");
        }
        daemon::DaemonBootEvent::OutboundTailRecovered {
            movements,
            hydrated_rows,
        } => boot.stage(
            "recovery",
            format_args!("{movements} outbound tail movement(s) restored · {hydrated_rows} exact row(s) decoded"),
        ),
        daemon::DaemonBootEvent::SpotlightOpening => {
            boot.stage("spotlight", "verifying movement index");
        }
        daemon::DaemonBootEvent::SpotlightReady { indexed_movements } => match indexed_movements {
            Some(count) => boot.stage("spotlight", format_args!("{count} movements indexed")),
            None => boot.stage("spotlight", "unavailable · JSONL remains authoritative"),
        },
        daemon::DaemonBootEvent::StateRebuilding => {
            boot.stage("state", "rebuilding durable run and EE projection");
        }
        daemon::DaemonBootEvent::StateRebuilt {
            durable_runs,
            ee_scopes,
        } => boot.stage(
            "state",
            format_args!("{durable_runs} durable run(s) · {ee_scopes} EE scope(s)"),
        ),
    }
}
