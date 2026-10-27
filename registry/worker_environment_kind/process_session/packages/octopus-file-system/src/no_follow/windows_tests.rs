use super::*;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE;
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::CreateNamedPipeW;
use windows_sys::Win32::System::Pipes::PIPE_READMODE_BYTE;
use windows_sys::Win32::System::Pipes::PIPE_TYPE_BYTE;
use windows_sys::Win32::System::Pipes::PIPE_WAIT;

#[test]
fn native_open_rejects_named_pipes_before_connecting() {
    let pipe_name = format!("codex-no-follow-{}", uuid::Uuid::new_v4());
    let server_path = PathBuf::from(format!(r"\\.\pipe\{pipe_name}"));
    let client_path = PathBuf::from(format!(r"\\localhost\pipe\{pipe_name}"));
    let wide_path = server_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let pipe = unsafe {
        CreateNamedPipeW(
            wide_path.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            1,
            1,
            1_000,
            ptr::null(),
        )
    };
    assert_ne!(pipe, INVALID_HANDLE_VALUE);
    let _pipe = unsafe { OwnedHandle::from_raw_handle(pipe as RawHandle) };

    let error = open_handle(
        &client_path,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
    )
    .expect_err("strict native open should reject named pipes");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        error.to_string(),
        "path names a named pipe or mailslot, not a file"
    );
}

/// A drive letter is an object-manager link, so a strict open of a whole
/// `C:\...` path would refuse every file. Plain paths must work.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn plain_drive_paths_round_trip() {
    block_on(plain_drive_paths_round_trip_async());
}

async fn plain_drive_paths_round_trip_async() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("nested").join("deeper");
    create_directory(dir.clone(), true).await.unwrap();
    let file = dir.join("file.txt");
    write_file(file.clone(), b"hello".to_vec()).await.unwrap();
    assert!(metadata(file.clone()).await.unwrap().is_file());
    let mut opened = open_file(file.clone()).await.unwrap().into_std().await;
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut opened, &mut contents).unwrap();
    drop(opened);
    assert_eq!(contents, "hello");
    remove(file.clone(), false, false).await.unwrap();
    assert_eq!(
        metadata(file).await.unwrap_err().kind(),
        io::ErrorKind::NotFound
    );

    // The verbatim spelling names the same file.
    let verbatim = temp.path().canonicalize().unwrap().join("verbatim.txt");
    write_file(verbatim.clone(), b"v".to_vec()).await.unwrap();
    assert_eq!(std::fs::read(&verbatim).unwrap(), b"v");
}

#[test]
fn a_junction_below_the_volume_root_is_refused() {
    block_on(a_junction_below_the_volume_root_is_refused_async());
}

async fn a_junction_below_the_volume_root_is_refused_async() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let link = temp.path().join("link");
    let status = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&link)
        .arg(&target)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J needs no privilege");

    let error = write_file(link.join("escaped.txt"), b"x".to_vec())
        .await
        .expect_err("a write through a junction must be refused");
    assert_eq!(error.to_string(), "path contains a reparse point");
    assert!(!target.join("escaped.txt").exists());
}
