use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use octopus_descriptors::ProcessSandboxMode;
use octopus_git_sandbox::{GitWorktreeProcessLimits, GitWorktreeProcessManager, GitWorktreeProcessStore};
use octopus_worker_env::{AbsPath, NetworkCeiling, ProjectedSandbox, WorkerEnvironmentId};

#[tokio::test(flavor = "multi_thread")]
async fn a_room_command_outlives_its_tool_process_and_the_next_one_receives_its_exact_outcome() {
    let temporary = tempfile::Builder::new().prefix("keeper").tempdir_in("/tmp").unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    for directory in ["durable", "ipc", "work"] {
        std::fs::create_dir(root.join(directory)).unwrap();
    }
    let store = GitWorktreeProcessStore::new(
        &root.join("durable"),
        &root.join("ipc"),
        PathBuf::from(env!("CARGO_BIN_EXE_octopus-workspace-exec-tool")),
    )
    .unwrap();
    let cwd = AbsPath::new(&root.join("work")).unwrap();
    let sandbox = ProjectedSandbox::from_parts(
        vec![cwd.clone()],
        Vec::new(),
        vec![AbsPath::new(Path::new("/")).unwrap()],
        NetworkCeiling::Denied,
    );
    let room = WorkerEnvironmentId::derive(b"keeper-contract");
    let limits = GitWorktreeProcessLimits {
        max_processes_per_worker_environment: 2,
        max_buffered_output_bytes: 4096,
        max_buffered_output_events: 64,
        max_input_bytes: 64,
        process_timeout: Duration::from_secs(30),
        cleanup_timeout: Duration::from_secs(5),
    };
    let text = |poll: &octopus_git_sandbox::GitWorktreeProcessPoll| {
        poll.chunks.iter().flat_map(|chunk| chunk.bytes.clone()).collect::<Vec<_>>()
    };

    let first_owner = GitWorktreeProcessManager::new(Some(store.clone()));
    let started = start(&first_owner, &room, &sandbox, &cwd, &limits).await.unwrap();
    assert!(started.running && !started.closed);
    assert_eq!(text(&started), b"started");
    assert_eq!(start(&first_owner, &room, &sandbox, &cwd, &limits).await.unwrap(), started, "a re-invoked start returns its exact result and starts nothing");
    let written = first_owner
        .write_and_poll(&room, &started.process_id, b"SURVIVED\n", 0, 1024, Some("movement:11"))
        .await
        .unwrap();
    assert!(written.running && written.chunks.is_empty());
    drop(first_owner);

    let next_owner = GitWorktreeProcessManager::new(Some(store));
    assert_eq!(
        next_owner
            .write_and_poll(&room, &started.process_id, b"SURVIVED\n", 0, 1024, Some("movement:11"))
            .await
            .unwrap(),
        written,
        "a re-invoked write is answered exactly and writes nothing twice"
    );
    let finished = next_owner
        .write_and_poll(&room, &started.process_id, b"", 10_000, 1024, Some("movement:12"))
        .await
        .unwrap();
    assert_eq!(text(&finished), b" SURVIVED");
    assert_eq!((finished.exit_code, finished.closed, finished.continuation_available), (Some(7), true, false));
    assert_eq!(
        next_owner
            .write_and_poll(&room, &started.process_id, b"", 0, 1024, Some("movement:12"))
            .await
            .unwrap(),
        finished
    );
    assert!(next_owner.list(&room).await.unwrap().is_empty());
    next_owner.release(&room, None).await.unwrap();
    assert!(next_owner
        .write_and_poll(&room, &started.process_id, b"", 0, 1024, Some("movement:13"))
        .await
        .unwrap_err()
        .contains("has no persistent process"));
}

async fn start(
    owner: &GitWorktreeProcessManager,
    room: &WorkerEnvironmentId,
    sandbox: &ProjectedSandbox,
    cwd: &AbsPath,
    limits: &GitWorktreeProcessLimits,
) -> Result<octopus_git_sandbox::GitWorktreeProcessPoll, String> {
    owner
        .start(
            room,
            sandbox,
            ProcessSandboxMode::Disabled,
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf started; IFS= read line; sleep 1; printf ' %s' \"$line\"; exit 7".to_string(),
            ],
            cwd.as_path().to_path_buf(),
            BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
            false,
            300,
            1024,
            limits.clone(),
            Some("movement:10"),
        )
        .await
}
