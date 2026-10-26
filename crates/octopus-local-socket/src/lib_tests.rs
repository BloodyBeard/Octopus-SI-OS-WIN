use std::io::ErrorKind;

use pretty_assertions::assert_eq;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use super::*;

#[tokio::test]
async fn prepare_private_socket_directory_creates_directory() {
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let socket_dir = temp_dir.path().join("app-server-control");

    prepare_private_socket_directory(&socket_dir)
        .await
        .expect("socket dir should be created");

    assert!(socket_dir.is_dir());
}

#[cfg(unix)]
#[tokio::test]
async fn prepare_private_socket_directory_sets_existing_permissions_to_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    for mode in [0o755, 0o600] {
        let socket_dir = temp_dir.path().join(format!("app-server-control-{mode:o}"));
        std::fs::create_dir(&socket_dir).expect("socket dir should be created");
        std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(mode))
            .expect("socket dir permissions should be changed");

        prepare_private_socket_directory(&socket_dir)
            .await
            .expect("socket dir permissions should be set exactly");

        let mode = std::fs::metadata(&socket_dir)
            .expect("socket dir metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn regular_file_path_is_not_stale_socket_path() {
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let regular_file = temp_dir.path().join("not-a-socket");
    std::fs::write(&regular_file, b"not a socket").expect("regular file should be created");

    assert!(
        !is_stale_socket_path(&regular_file)
            .await
            .expect("stale socket check should succeed")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bound_listener_path_is_stale_socket_path() {
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let socket_path = temp_dir.path().join("socket");
    let _listener = match UnixListener::bind(&socket_path).await {
        Ok(listener) => listener,
        Err(err) if err.kind() == ErrorKind::PermissionDenied => {
            eprintln!("skipping test: failed to bind unix socket: {err}");
            return;
        }
        Err(err) => panic!("failed to bind test socket: {err}"),
    };

    assert!(
        is_stale_socket_path(&socket_path)
            .await
            .expect("stale socket check should succeed")
    );
}

#[tokio::test]
async fn stream_round_trips_data_between_listener_and_client() {
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let socket_path = temp_dir.path().join("socket");
    let mut listener = match UnixListener::bind(&socket_path).await {
        Ok(listener) => listener,
        Err(err) if err.kind() == ErrorKind::PermissionDenied => {
            eprintln!("skipping test: failed to bind unix socket: {err}");
            return;
        }
        Err(err) => panic!("failed to bind test socket: {err}"),
    };

    let server_task = tokio::spawn(async move {
        let mut server_stream = listener.accept().await.expect("connection should accept");
        let mut request = [0; 7];
        server_stream
            .read_exact(&mut request)
            .await
            .expect("server should read request");
        assert_eq!(&request, b"request");
        server_stream
            .write_all(b"response")
            .await
            .expect("server should write response");
    });

    let mut client_stream = UnixStream::connect(&socket_path)
        .await
        .expect("client should connect");
    client_stream
        .write_all(b"request")
        .await
        .expect("client should write request");
    let mut response = [0; 8];
    client_stream
        .read_exact(&mut response)
        .await
        .expect("client should read response");
    assert_eq!(&response, b"response");

    server_task.await.expect("server task should join");
}

#[test]
fn pipe_name_ignores_verbatim_prefix_separators_and_case() {
    let expected = r"\\.\pipe\octopus-bd8b7f095da0d32b198bed7bb31f63c4";
    for path in [
        r"C:\Octopus\Home\app-server-control\abc.sock",
        r"\\?\C:\Octopus\Home\app-server-control\abc.sock",
        "c:/octopus/home/app-server-control/abc.sock",
    ] {
        assert_eq!(pipe_name(std::path::Path::new(path)), expected);
    }
}

#[cfg(windows)]
#[tokio::test]
async fn second_listener_on_a_path_is_refused() {
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let socket_path = temp_dir.path().join("socket");
    let _listener = UnixListener::bind(&socket_path).await.expect("first bind");
    let error = UnixListener::bind(&socket_path)
        .await
        .err()
        .expect("second bind must fail");
    assert_eq!(error.kind(), ErrorKind::AddrInUse);
    assert!(!is_stale_socket_path(&socket_path).await.unwrap());
}
