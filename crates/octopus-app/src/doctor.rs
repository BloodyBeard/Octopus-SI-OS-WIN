use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use octopus_descriptors::InterfaceId;
use octopus_interface_transport::{
    RESOLVED_INTERFACE_DESCRIPTOR_ENV, ResolvedSpawnLifeExecutable, SpawnLifeDoctorCommand,
    resolve_spawn_life_executable, validate_spawn_life_config,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const PROBE_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;
const PROBE_CONCURRENCY: usize = 8;

struct ProbeJob {
    interface: InterfaceId,
    process_index: usize,
    interface_dir: PathBuf,
    descriptor_json: String,
    executable: ResolvedSpawnLifeExecutable,
    doctor: SpawnLifeDoctorCommand,
}

struct ProbeOutcome {
    interface: InterfaceId,
    process_index: usize,
    result: Result<(), String>,
}

#[derive(Deserialize)]
struct ClientDoctorReport {
    schema_version: u64,
    checks: usize,
    passed: usize,
    findings: Vec<ClientDoctorFinding>,
    errors: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct ClientDoctorFinding {
    code: String,
    subject: String,
    reason: String,
    next_step: String,
}

struct ClientDoctorLayout {
    script: PathBuf,
    source_registry_root: PathBuf,
    library_root: PathBuf,
    runner: ClientDoctorRunner,
}

enum ClientDoctorRunner {
    Electron(PathBuf),
    Node,
}

pub async fn run(octopus_home: &Path, installation_root: &Path) -> Result<(), String> {
    let registry_root = octopus_home.join("registry");
    let interface_root = registry_root.join("interface");
    require_plain_directory(&registry_root, "active registry")?;
    require_plain_directory(&interface_root, "active interface registry")?;

    let installation_root = std::fs::canonicalize(installation_root).map_err(|error| {
        format!(
            "installation root '{}' cannot be canonicalized: {error}",
            installation_root.display()
        )
    })?;
    let active_interface_root = std::fs::canonicalize(&interface_root).map_err(|error| {
        format!(
            "active interface registry '{}' cannot be canonicalized: {error}",
            interface_root.display()
        )
    })?;
    let packaged_interface_root = installation_root
        .join("local-packages")
        .join("registry")
        .join("interface");
    let packaged_interface_root = packaged_interface_root
        .is_dir()
        .then(|| std::fs::canonicalize(&packaged_interface_root))
        .transpose()
        .map_err(|error| {
            format!(
                "packaged interface registry '{}' cannot be canonicalized: {error}",
                packaged_interface_root.display()
            )
        })?;

    let (interfaces, mut errors) = discover_interfaces(&interface_root)?;
    match diagnose_run_worker_policies(octopus_home, &registry_root) {
        Ok(issues) => errors.extend(issues),
        Err(error) => errors.push(format!("run worker policies: {error}")),
    }
    let mut life_processes = 0usize;
    let mut unprobed_processes = 0usize;
    let mut probe_jobs = Vec::new();
    let mut executable_uses = BTreeMap::<(PathBuf, PathBuf), BTreeSet<String>>::new();
    let mut imported_executable_cache =
        BTreeMap::<String, Result<ResolvedSpawnLifeExecutable, String>>::new();

    for interface in &interfaces {
        let resolved = match validate_spawn_life_config(&registry_root, interface) {
            Ok(resolved) => resolved,
            Err(error) => {
                errors.push(format!("interface '{}': {error}", interface.as_str()));
                continue;
            }
        };
        let Some(resolved) = resolved else {
            continue;
        };
        let descriptor_json =
            serde_json::to_string(&resolved.interface_descriptor).map_err(|error| {
                format!(
                    "interface '{}' resolved descriptor cannot be encoded: {error}",
                    interface.as_str()
                )
            })?;
        for (process_index, process) in resolved.config.processes.iter().enumerate() {
            life_processes += 1;
            let executable_result = if process.bin.starts_with("interface://") {
                if let Some(cached) = imported_executable_cache.get(&process.bin) {
                    cached.clone()
                } else {
                    let resolved = resolve_spawn_life_executable(
                        &registry_root,
                        interface,
                        &installation_root,
                        process,
                    );
                    imported_executable_cache.insert(process.bin.clone(), resolved.clone());
                    resolved
                }
            } else {
                resolve_spawn_life_executable(
                    &registry_root,
                    interface,
                    &installation_root,
                    process,
                )
            };
            let executable = match executable_result {
                Ok(executable) => executable,
                Err(error) => {
                    errors.push(format!(
                        "interface '{}' process {process_index}: {error}",
                        interface.as_str()
                    ));
                    continue;
                }
            };
            if let Some(packaged_root) = &packaged_interface_root {
                if let Some(relative) = active_registry_relative_executable(
                    &active_interface_root,
                    &executable.executable,
                ) {
                    let packaged = packaged_root.join(relative);
                    if packaged.is_file() {
                        executable_uses
                            .entry((executable.executable.clone(), packaged))
                            .or_default()
                            .insert(format!("{}#{process_index}", interface.as_str()));
                    }
                }
            }
            let Some(doctor) = process.doctor.clone() else {
                unprobed_processes += 1;
                continue;
            };
            probe_jobs.push(ProbeJob {
                interface: interface.clone(),
                process_index,
                interface_dir: interface_root.join(interface.as_str()),
                descriptor_json: descriptor_json.clone(),
                executable,
                doctor,
            });
        }
    }

    let mut executable_pairs = 0usize;
    for ((active, packaged), users) in executable_uses {
        executable_pairs += 1;
        if let Err(error) = compare_executables(&active, &packaged) {
            let users = users.into_iter().collect::<Vec<_>>().join(", ");
            errors.push(format!("packaged executable drift for {users}: {error}"));
        }
    }

    let (client_doctor, client_doctor_execution_failed) =
        match run_client_doctor(octopus_home, &installation_root).await {
            Ok(report) => (report, false),
            Err(error) => {
                errors.push(format!("client package doctor: {error}"));
                (None, true)
            }
        };
    if let Some(report) = &client_doctor {
        errors.extend(
            report
                .findings
                .iter()
                .map(|finding| format!("client package check {}", render_client_finding(finding))),
        );
    }

    let declared_probes = probe_jobs.len();
    let mut outcomes = run_probes(probe_jobs).await;
    outcomes.sort_by(|left, right| {
        (&left.interface, left.process_index).cmp(&(&right.interface, right.process_index))
    });
    let mut passed_probes = 0usize;
    for outcome in outcomes {
        match outcome.result {
            Ok(()) => passed_probes += 1,
            Err(error) => errors.push(format!(
                "interface '{}' process {} doctor: {error}",
                outcome.interface.as_str(),
                outcome.process_index
            )),
        }
    }

    errors.sort();
    println!("octopus doctor");
    println!("home       {}", octopus_home.display());
    println!(
        "registry   {} interfaces · {} life processes",
        interfaces.len(),
        life_processes
    );
    if packaged_interface_root.is_some() {
        println!("packages   {executable_pairs} active executable pairs checked");
    } else {
        println!("packages   no packaged local registry in this installation");
    }
    match (&client_doctor, client_doctor_execution_failed) {
        (Some(report), _) => println!(
            "client     {}/{} read-only structured checks passed",
            report.passed, report.checks
        ),
        (None, true) => println!("client     package checks failed to run"),
        (None, false) => println!("client     no client-owned package checks available"),
    }
    println!(
        "probes     {passed_probes}/{declared_probes} passed · {unprobed_processes} not declared"
    );
    if errors.is_empty() {
        println!("result     PASS");
        return Ok(());
    }
    println!("result     FAIL · {} error(s)", errors.len());
    for error in &errors {
        eprintln!("- {error}");
    }
    Err(format!("octopus doctor found {} error(s)", errors.len()))
}

async fn run_client_doctor(
    octopus_home: &Path,
    installation_root: &Path,
) -> Result<Option<ClientDoctorReport>, String> {
    let Some(layout) = client_doctor_layout(installation_root)? else {
        return Ok(None);
    };
    let mut command = match &layout.runner {
        ClientDoctorRunner::Electron(executable) => {
            let mut command = tokio::process::Command::new(executable);
            command.env_clear().env("ELECTRON_RUN_AS_NODE", "1");
            command
        }
        ClientDoctorRunner::Node => tokio::process::Command::new("node"),
    };
    command
        .arg(&layout.script)
        .arg(&layout.source_registry_root)
        .arg(octopus_home.join("registry"))
        .arg(&layout.library_root)
        .arg(octopus_home.join("client/package-state"))
        .current_dir(installation_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        format!(
            "client doctor runner for '{}' could not start: {error}",
            layout.script.display()
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "client doctor stdout is unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "client doctor stderr is unavailable".to_string())?;
    let wait = async {
        match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
            Ok(status) => status.map_err(|error| format!("client doctor wait failed: {error}")),
            Err(_) => {
                child.kill().await.map_err(|error| {
                    format!("timed-out client doctor could not be killed: {error}")
                })?;
                let _ = child.wait().await;
                Err("timed out after 30000 ms".to_string())
            }
        }
    };
    let (status, stdout, stderr) = tokio::join!(
        wait,
        drain_bounded_output(stdout),
        drain_bounded_output(stderr)
    );
    let stdout = stdout?;
    let stderr = stderr?;
    let status = status?;
    let report = serde_json::from_slice::<ClientDoctorReport>(&stdout).map_err(|error| {
        let diagnostic = if !stderr.is_empty() {
            render_diagnostic(&stderr)
        } else if !stdout.is_empty() {
            render_diagnostic(&stdout)
        } else {
            "no diagnostic output".to_string()
        };
        format!("client doctor returned invalid JSON ({error}): {diagnostic}")
    })?;
    if report.schema_version != 2
        || report.checks == 0
        || report.passed > report.checks
        || report.checks - report.passed != report.errors.len()
        || report.findings.len() != report.errors.len()
    {
        return Err("client doctor returned an invalid report contract".to_string());
    }
    if status.success() != report.errors.is_empty() {
        return Err(format!(
            "client doctor exit status {status} disagrees with its report"
        ));
    }
    Ok(Some(report))
}

fn render_client_finding(finding: &ClientDoctorFinding) -> String {
    format!(
        "{} [{}]: {} Next manual step: {}",
        finding.code, finding.subject, finding.reason, finding.next_step
    )
}

fn client_doctor_layout(installation_root: &Path) -> Result<Option<ClientDoctorLayout>, String> {
    let packaged_script = installation_root.join("electron/clientDoctor.cjs");
    if packaged_script.is_file() {
        let executable = installation_root
            .parent()
            .and_then(Path::parent)
            .map(|contents| contents.join("MacOS/Octopus"))
            .ok_or_else(|| {
                format!(
                    "packaged installation '{}' has no application Contents root",
                    installation_root.display()
                )
            })?;
        if !executable.is_file() {
            return Err(format!(
                "packaged client doctor runner is missing: {}",
                executable.display()
            ));
        }
        return Ok(Some(ClientDoctorLayout {
            script: packaged_script,
            source_registry_root: installation_root.join("local-packages/registry"),
            library_root: installation_root.join("local-packages"),
            runner: ClientDoctorRunner::Electron(executable),
        }));
    }
    if installation_root.join("local-packages/registry").is_dir() {
        return Err(format!(
            "packaged client doctor script is missing: {}",
            packaged_script.display()
        ));
    }
    let development_script = installation_root.join("app/electron/clientDoctor.cjs");
    if development_script.is_file() {
        return Ok(Some(ClientDoctorLayout {
            script: development_script,
            source_registry_root: installation_root.join("registry"),
            library_root: installation_root.join("app/distribution/local-packages"),
            runner: ClientDoctorRunner::Node,
        }));
    }
    Ok(None)
}

fn require_plain_directory(path: &Path, label: &str) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("{label} '{}' cannot be inspected: {error}", path.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "{label} '{}' is not a plain directory",
            path.display()
        ));
    }
    Ok(())
}

fn diagnose_run_worker_policies(home: &Path, registry_root: &Path) -> Result<Vec<String>, String> {
    let host_path = home.join("host.json");
    let host_bytes = match std::fs::read(&host_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", host_path.display())),
    };
    let host: serde_json::Value = serde_json::from_slice(&host_bytes)
        .map_err(|error| format!("{}: {error}", host_path.display()))?;
    if host["host_manifest_version"].as_u64() != Some(1) {
        return Err(format!("{}: unsupported host manifest version", host_path.display()));
    }
    let runs_root = host["root_refs"]["run_records_root"].as_str().map(Path::new)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| format!("{}: run_records_root must be an absolute path", host_path.display()))?;
    let entries = match std::fs::read_dir(runs_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", runs_root.display())),
    };
    let mut owners = BTreeMap::<InterfaceId, BTreeSet<String>>::new();
    let mut issues = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", runs_root.display()))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let read = || -> Result<octopus_runtime::run_record::RunRecord, String> {
            if !entry.file_type().map_err(|error| error.to_string())?.is_file() {
                return Err("run record must be a regular file".into());
            }
            let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
            let record = octopus_runtime::run_record::RunRecord::from_bytes(&bytes)
                .map_err(|error| error.to_string())?;
            if path.file_stem().and_then(|stem| stem.to_str()) != Some(record.id.as_str()) {
                return Err("run identity differs from its filename".into());
            }
            Ok(record)
        };
        match read() {
            Ok(record) => {
                if record.active_checkpoints.iter().any(|id| record.checkpoint(id).is_some_and(|checkpoint|
                    checkpoint.interface_binding.is_some() || !checkpoint.outbound_interface_bindings.is_empty())) {
                    owners.entry(record.worker).or_default().insert(record.id.to_string());
                }
            }
            Err(error) => issues.push(format!("run record '{}': {error}", path.display())),
        }
    }
    if !owners.is_empty() {
        let workers = owners.keys().cloned().collect::<Vec<_>>();
        for issue in octopus_app_server::registry_worker_policy_issues(registry_root, &workers)? {
            let worker = issue.subject.strip_prefix("interface/")
                .ok_or("worker policy diagnostic has no interface subject")?;
            let runs = owners.get(&InterfaceId::new(worker))
                .ok_or("worker policy diagnostic names an unselected worker")?
                .iter().cloned().collect::<Vec<_>>().join(", ");
            issues.push(format!("worker '{}' bound by runs [{runs}]: {}", worker, issue.reason));
        }
    }
    issues.sort();
    Ok(issues)
}

fn discover_interfaces(interface_root: &Path) -> Result<(Vec<InterfaceId>, Vec<String>), String> {
    let mut entries = std::fs::read_dir(interface_root)
        .map_err(|error| {
            format!(
                "active interface registry '{}' cannot be read: {error}",
                interface_root.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            format!(
                "active interface registry '{}' cannot be enumerated: {error}",
                interface_root.display()
            )
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut interfaces = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => {
                errors.push(format!(
                    "interface registry entry '{}' is not valid UTF-8",
                    entry.path().display()
                ));
                continue;
            }
        };
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                errors.push(format!(
                    "interface registry entry '{}' cannot be inspected: {error}",
                    entry.path().display()
                ));
                continue;
            }
        };
        if name == octopus_file_storage::publication::COORDINATION_DIRECTORY {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                errors.push(format!("publication metadata '{}' is not a plain directory", entry.path().display()));
            }
            continue;
        }
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            interfaces.push(InterfaceId::new(name));
        } else if !metadata.is_file() || metadata.file_type().is_symlink() {
            errors.push(format!(
                "interface registry entry '{}' is not a plain directory",
                entry.path().display()
            ));
        }
    }
    Ok((interfaces, errors))
}

fn active_registry_relative_executable<'a>(
    active_interface_root: &'a Path,
    executable: &'a Path,
) -> Option<&'a Path> {
    executable.strip_prefix(active_interface_root).ok()
}

fn compare_executables(active: &Path, packaged: &Path) -> Result<(), String> {
    let active_metadata = std::fs::metadata(active).map_err(|error| {
        format!(
            "active executable '{}' cannot be inspected: {error}",
            active.display()
        )
    })?;
    let packaged_metadata = std::fs::metadata(packaged).map_err(|error| {
        format!(
            "packaged executable '{}' cannot be inspected: {error}",
            packaged.display()
        )
    })?;
    if active_metadata.len() != packaged_metadata.len() {
        return Err(format!(
            "active '{}' has {} bytes, packaged '{}' has {} bytes",
            active.display(),
            active_metadata.len(),
            packaged.display(),
            packaged_metadata.len()
        ));
    }
    let active_hash = sha256_file(active)?;
    let packaged_hash = sha256_file(packaged)?;
    if active_hash != packaged_hash {
        return Err(format!(
            "active '{}' SHA-256 {active_hash} differs from packaged '{}' SHA-256 {packaged_hash}",
            active.display(),
            packaged.display()
        ));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("executable '{}' cannot be opened: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("executable '{}' cannot be read: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

async fn run_probes(jobs: Vec<ProbeJob>) -> Vec<ProbeOutcome> {
    let semaphore = Arc::new(Semaphore::new(PROBE_CONCURRENCY));
    let mut tasks = JoinSet::new();
    for job in jobs {
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            let permit = semaphore
                .acquire_owned()
                .await
                .map_err(|_| "doctor probe scheduler closed".to_string());
            let result = match permit {
                Ok(_permit) => run_probe(&job).await,
                Err(error) => Err(error),
            };
            ProbeOutcome {
                interface: job.interface,
                process_index: job.process_index,
                result,
            }
        });
    }
    let mut outcomes = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) => outcomes.push(ProbeOutcome {
                interface: InterfaceId::new("doctor_probe_task"),
                process_index: 0,
                result: Err(format!("probe task failed: {error}")),
            }),
        }
    }
    outcomes
}

async fn run_probe(job: &ProbeJob) -> Result<(), String> {
    let mut command = tokio::process::Command::new(&job.executable.executable);
    command
        .args(&job.doctor.args)
        .current_dir(&job.executable.working_directory)
        .env_clear()
        .env("OCTOPUS_INTERFACE_ID", job.interface.as_str())
        .env("OCTOPUS_INTERFACE_DIR", &job.interface_dir)
        .env(RESOLVED_INTERFACE_DESCRIPTOR_ENV, &job.descriptor_json)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        format!(
            "executable '{}' could not start: {error}",
            job.executable.executable.display()
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "doctor probe stdout is unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "doctor probe stderr is unavailable".to_string())?;
    let wait = async {
        match tokio::time::timeout(Duration::from_millis(job.doctor.timeout_ms), child.wait()).await
        {
            Ok(status) => status.map_err(|error| format!("probe wait failed: {error}")),
            Err(_) => {
                child
                    .kill()
                    .await
                    .map_err(|error| format!("timed-out probe could not be killed: {error}"))?;
                let _ = child.wait().await;
                Err(format!("timed out after {} ms", job.doctor.timeout_ms))
            }
        }
    };
    let (status, stdout, stderr) = tokio::join!(
        wait,
        drain_bounded_output(stdout),
        drain_bounded_output(stderr)
    );
    let stdout = stdout?;
    let stderr = stderr?;
    let status = status?;
    if status.success() {
        return Ok(());
    }
    let diagnostic = if !stderr.is_empty() {
        render_diagnostic(&stderr)
    } else if !stdout.is_empty() {
        render_diagnostic(&stdout)
    } else {
        "no diagnostic output".to_string()
    };
    Err(format!("exited with {status}: {diagnostic}"))
}

async fn drain_bounded_output<R>(mut reader: R) -> Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut retained = Vec::new();
    let mut observed = 0usize;
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| format!("doctor probe output could not be read: {error}"))?;
        if read == 0 {
            break;
        }
        observed = observed.saturating_add(read);
        if retained.len() < PROBE_OUTPUT_LIMIT_BYTES {
            let available = PROBE_OUTPUT_LIMIT_BYTES - retained.len();
            retained.extend_from_slice(&buffer[..read.min(available)]);
        }
    }
    if observed > PROBE_OUTPUT_LIMIT_BYTES {
        return Err(format!(
            "doctor probe emitted {observed} bytes, exceeding the {} byte ceiling",
            PROBE_OUTPUT_LIMIT_BYTES
        ));
    }
    Ok(retained)
}

fn render_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn doctor_reports_bound_worker_loader_refusals_and_isolates_unused_presets() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let registry = home.join("registry");
        let runs = root.path().join("authored-runs");
        let installation = root.path().join("installation");
        for path in [runs.clone(), installation.clone(), registry.join("tool"),
            registry.join("worker_environment_kind"), registry.join("ee_template")] {
            std::fs::create_dir_all(path).unwrap();
        }
        for (worker, tools) in [("system_worker_valid", serde_json::json!([])),
            ("system_worker_optional", serde_json::json!(["missing.private.tool"]))] {
            let folder = registry.join("interface").join(worker);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("interface.json"), serde_json::json!({
                "id": worker, "interface_type": "system", "role": "worker",
                "direction": "bidirectional", "source_names": ["worker"]
            }).to_string()).unwrap();
            std::fs::write(folder.join("policy.json"), serde_json::json!({
                "id": worker, "ceiling": {"interfaces": ["uninstalled_optional_interface"],
                    "tools": tools, "wek": [], "mark_outcomes": [], "hard_blocked_tools": []},
                "role_artifacts": []
            }).to_string()).unwrap();
        }
        std::fs::write(home.join("host.json"), serde_json::json!({
            "host_manifest_version": 1, "root_refs": {"run_records_root": runs}
        }).to_string()).unwrap();
        let mut record = serde_json::json!({
            "id": "run_bound", "ee": "ee_test", "worker": "system_worker_optional",
            "trace": "trace_test", "status": "idle", "active_checkpoints": [],
            "checkpoints": [{"id": "cp_test", "run": "run_bound", "trace": "trace_test",
                "epoch": 0, "interface_binding": {"interface": "system_worker_optional",
                    "config": {"artifact_policy": {"mode": "all"}}}}]
        });
        let run_path = runs.join("run_bound.json");
        std::fs::write(&run_path, record.to_string()).unwrap();
        run(&home, &installation).await.unwrap();

        record["active_checkpoints"] = serde_json::json!(["cp_test"]);
        std::fs::write(&run_path, record.to_string()).unwrap();
        let issues = diagnose_run_worker_policies(&home, &registry).unwrap();
        assert_eq!(issues.len(), 1, "{issues:?}");
        for expected in ["system_worker_optional", "run_bound", "ceiling.tools", "missing.private.tool"] {
            assert!(issues[0].contains(expected), "{issues:?}");
        }
        assert!(run(&home, &installation).await.is_err());

        record["worker"] = serde_json::json!("system_worker_valid");
        record["checkpoints"][0]["interface_binding"]["interface"] = serde_json::json!("system_worker_valid");
        std::fs::write(&run_path, record.to_string()).unwrap();
        run(&home, &installation).await.unwrap();
        assert_eq!(std::fs::read_to_string(&run_path).unwrap(), record.to_string());
        assert!(!home.join("state").exists());
        assert!(!home.join("runtime-instance.json").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn doctor_runs_a_declared_probe_without_materializing_runtime_state() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let interface = home.join("registry/interface/world_test");
        std::fs::create_dir_all(&interface).unwrap();
        std::fs::write(home.join("registry/interface/GUIDE.md"), "registry guide\n").unwrap();
        std::fs::write(
            interface.join("interface.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "world_test",
                "interface_type": "world",
                "role": "service",
                "direction": "bidirectional",
                "source_names": ["test"],
                "interface_driver_ref": "test_driver"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            interface.join("life.json"),
            serde_json::to_vec(&serde_json::json!({
                "processes": [{
                    "bin": "/usr/bin/true",
                    "io_limits": {
                        "max_control_frame_bytes": 1024,
                        "max_stderr_bytes": 1024,
                        "max_manifest_bytes": 1024,
                        "max_payload_bytes": 1024
                    },
                    "doctor": {"args": [], "timeout_ms": 1000},
                    "authority": {
                        "read_root_slots": [],
                        "write_root_slots": [],
                        "executable_root_slots": [],
                        "read_paths": [],
                        "write_paths": [],
                        "executable_paths": ["/usr/bin/true"],
                        "executable_commands": [],
                        "read_env_paths": [],
                        "write_env_paths": [],
                        "executable_env_paths": [],
                        "read_env_subpaths": [],
                        "write_env_subpaths": [],
                        "executable_env_subpaths": [],
                        "allow_network": false,
                        "allow_local_ipc": false
                    },
                    "read_handles": []
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let installation_root = root.path().join("installation");
        std::fs::create_dir(&installation_root).unwrap();
        let metadata = home.join("registry/interface").join(octopus_file_storage::publication::COORDINATION_DIRECTORY);
        std::fs::create_dir(&metadata).unwrap();

        run(&home, &installation_root).await.unwrap();

        std::fs::remove_dir(&metadata).unwrap();
        std::os::unix::fs::symlink(&interface, &metadata).unwrap();
        let (_, errors) = discover_interfaces(&home.join("registry/interface")).unwrap();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("not a plain directory"));

        assert!(!home.join("state").exists());
        assert!(!home.join("runtime-instance.json").exists());
    }
}
