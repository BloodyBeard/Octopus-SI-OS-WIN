// Desktop control runs a macOS host and checks Unix file modes.
#![cfg(unix)]

use octopus_system_gui_env::{
    backend::{DesktopExecutor, DesktopHost},
    protocol::{self, Request, Response, Screen},
};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

fn enabled_preferences(root: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.canonicalize().unwrap().join("preferences");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = path.join("computer-use.json");
    std::fs::write(&file, b"{\"schema_version\":1,\"enabled\":true}").unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

struct NativeFixture {
    posted: AtomicUsize,
    hold: AtomicBool,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl Default for NativeFixture {
    fn default() -> Self {
        Self {
            posted: AtomicUsize::new(0),
            hold: AtomicBool::new(false),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        }
    }
}

fn screen() -> Value {
    json!({"id":"left","x":-500.0,"y":0.0,"width":500.0,"height":300.0,"pixel_width":1000,"pixel_height":600})
}
fn native_status() -> Value {
    json!({"permissions":{"screen_capture":true,"accessibility":true},"screens":[screen()]})
}

impl DesktopExecutor for NativeFixture {
    fn execute<'a>(
        &'a self,
        action: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>> {
        Box::pin(async move {
            let mut result = native_status();
            if action["type"] == "view" {
                use base64::Engine;
                use sha2::Digest;
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::DynamicImage::new_rgb8(1000, 600)
                    .write_to(&mut bytes, image::ImageFormat::Png)
                    .unwrap();
                let bytes = bytes.into_inner();
                result["observation"] = json!({"id":"passive-frame","screen":screen(),"focus":null,"input_revision":"hardware-1","captured_at_unix_ms":1,"image":{"media_type":"image/png","width":1000,"height":600,"byte_length":bytes.len(),"sha256":format!("{:x}",sha2::Sha256::digest(&bytes)),"data_base64":base64::engine::general_purpose::STANDARD.encode(bytes)}});
            } else if action["type"] == "observe" {
                result["observation"] = json!({"id":"native","screen":screen(),"focus":{"app_id":"example.editor","process_id":17,"window_id":23},"input_revision":"hardware-1","captured_at_unix_ms":1,"image":null});
            } else if protocol::is_input(action["type"].as_str().unwrap_or("")) {
                assert_eq!(action["expected"]["screen"], screen());
                assert_eq!(action["point"], json!({"x":-250.0,"y":150.0}));
                if self.hold.load(Ordering::SeqCst) {
                    self.started.notify_one();
                    self.release.notified().await;
                }
                let cancelled =
                    std::path::Path::new(action["cancel_file"].as_str().unwrap()).exists();
                if !cancelled {
                    self.posted.fetch_add(1, Ordering::SeqCst);
                }
                result["completed"] = json!(!cancelled);
                result["events_posted"] = json!(if cancelled { 0 } else { 2 });
            }
            Ok(result)
        })
    }
}

fn request(room: &str, id: &str, action: Value) -> Request {
    Request {
        schema_version: 2,
        worker_environment_id: room.into(),
        request_id: id.into(),
        tool: "test.desktop".into(),
        action,
    }
}

async fn prepare(host: &DesktopHost, room: &str, id: &str) -> (String, u64) {
    let acquired = host
        .handle(
            request(
                room,
                &format!("{id}-control"),
                json!({"type":"control","mode":"acquire"}),
            ),
            false,
        )
        .await;
    assert!(acquired.accepted, "{acquired:?}");
    let generation = acquired.evidence.unwrap().control.generation;
    let observed = host
        .handle(
            request(
                room,
                &format!("{id}-observe"),
                json!({"type":"observe","screen_id":"left"}),
            ),
            false,
        )
        .await;
    assert!(observed.accepted, "{observed:?}");
    (
        observed.evidence.unwrap().observation.unwrap().id,
        generation,
    )
}

fn click(room: &str, id: &str, observation: &str, generation: u64) -> Request {
    request(
        room,
        id,
        json!({"type":"click","screen_id":"left","observation_id":observation,"control_generation":generation,"x":500,"y":300}),
    )
}

#[tokio::test]
async fn observed_input_is_owned_single_use_and_cancelled_without_waiting_for_executor() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Arc::new(NativeFixture::default());
    let host = Arc::new(DesktopHost::new(fixture.clone(), root.path().join("host")).unwrap().with_application_preferences(Some(enabled_preferences(root.path()))).unwrap());
    let (observation, generation) = prepare(&host, "room-a", "first").await;
    let view = host
        .handle(
            request(
                "room-a",
                "view",
                json!({"type":"view","view_id":"test","screen_id":"left"}),
            ),
            true,
        )
        .await
        .evidence
        .unwrap();
    let hash = &view.observation.unwrap().image.unwrap().sha256;
    let delta = host.handle(request("room-a","delta",json!({"type":"view","view_id":"test","screen_id":"left","known_image_sha256":hash})),true).await.evidence.unwrap();
    assert!(delta.observation.unwrap().image.is_none());
    assert_eq!(&delta.view_image_ref.unwrap().sha256, hash);
    let foreign = host
        .handle(click("room-b", "foreign", &observation, generation), false)
        .await;
    assert!(!foreign.accepted);
    assert_eq!(fixture.posted.load(Ordering::SeqCst), 0);
    let first = click("room-a", "first-click", &observation, generation);
    let result = host.handle(first.clone(), false).await;
    assert!(result.accepted, "{result:?}");
    protocol::validate_response(&result, &first).unwrap();
    assert!(
        !host
            .handle(
                click("room-a", "reused-observation", &observation, generation),
                false
            )
            .await
            .accepted
    );
    assert_eq!(fixture.posted.load(Ordering::SeqCst), 1);
    let (hover_observation, hover_generation) = prepare(&host, "room-a", "hover").await;
    let mut hover = click("room-a", "move", &hover_observation, hover_generation);
    hover.action["type"] = json!("move");
    let moved = host.handle(hover.clone(), false).await;
    assert!(moved.accepted);
    protocol::validate_response(&moved, &hover).unwrap();
    assert!(!host.handle(hover, false).await.accepted);
    let (observation, generation) = prepare(&host, "room-a", "second").await;
    fixture.hold.store(true, Ordering::SeqCst);
    let executing = {
        let host = host.clone();
        tokio::spawn(async move {
            host.handle(click("room-a", "held", &observation, generation), false)
                .await
        })
    };
    fixture.started.notified().await;
    let stopped = host
        .handle(
            request("room-a", "operator-stop", json!({"type":"stop"})),
            true,
        )
        .await;
    assert!(stopped.accepted);
    let stopped_control = stopped.evidence.unwrap().control;
    assert_eq!(stopped_control.mode, "stopped");
    assert!(!executing.is_finished());
    fixture.release.notify_one();
    assert!(!executing.await.unwrap().accepted);
    assert_eq!(fixture.posted.load(Ordering::SeqCst), 2);
    assert!(
        !host
            .handle(
                request(
                    "room-a",
                    "cannot-release-human",
                    json!({"type":"control","mode":"release"})
                ),
                false
            )
            .await
            .accepted
    );
    assert!(
        !host
            .handle(
                request(
                    "room-a",
                    "cannot-resume",
                    json!({"type":"control","mode":"acquire"})
                ),
                false
            )
            .await
            .accepted
    );
    assert!(
        host.handle(
            request(
                "room-a",
                "operator-release",
                json!({"type":"release_control","control_generation":stopped_control.generation})
            ),
            true
        )
        .await
        .accepted
    );
    assert!(
        host.handle(
            request(
                "room-a",
                "resume",
                json!({"type":"control","mode":"acquire"})
            ),
            false
        )
        .await
        .accepted
    );
}

#[tokio::test]
async fn stop_rejects_late_operator_release_or_takeover_under_the_same_control_lock() {
    let root = tempfile::tempdir().unwrap();
    let host = DesktopHost::new(Arc::new(NativeFixture::default()), root.path().join("host")).unwrap().with_application_preferences(Some(enabled_preferences(root.path()))).unwrap();
    let (_, before) = prepare(&host, "room", "start").await;
    let stopped = host.handle(request("room", "stop", json!({"type":"stop"})), true).await;
    assert!(stopped.accepted);
    let stopped_control = stopped.evidence.unwrap().control;
    for operation in ["release_control", "take_control"] {
        for generation in [Value::Null, json!(before)] {
            let denied = host.handle(request("room", operation, json!({"type":operation,"control_generation":generation})), true).await;
            assert!(!denied.accepted, "{denied:?}");
        }
    }
    let status = host.handle(request("room", "status", json!({"type":"status"})), true).await.evidence.unwrap();
    assert_eq!(status.control.mode, "stopped");
    assert_eq!(status.control.generation, stopped_control.generation);
    let released = host.handle(request("room", "fresh-release", json!({"type":"release_control","control_generation":stopped_control.generation})), true).await;
    assert!(released.accepted);
    assert_eq!(released.evidence.unwrap().control.mode, "idle");
    let stopped_again = host.handle(request("room", "stop-after-release", json!({"type":"stop"})), true).await;
    assert_eq!(stopped_again.evidence.unwrap().control.mode, "stopped");
}

#[tokio::test]
async fn physical_device_ownership_and_restart_require_explicit_recovery() {
    let root = tempfile::tempdir().unwrap();
    let device = root.path().join("physical-device.lock");
    let fixture = Arc::new(NativeFixture::default());
    let a = DesktopHost::new(fixture.clone(), root.path().join("a"))
        .unwrap()
        .with_application_preferences(Some(enabled_preferences(root.path()))).unwrap()
        .with_device_lock(device.clone())
        .unwrap();
    let b = DesktopHost::new(fixture.clone(), root.path().join("b"))
        .unwrap()
        .with_application_preferences(Some(enabled_preferences(root.path()))).unwrap()
        .with_device_lock(device)
        .unwrap();
    prepare(&a, "room-a", "first").await;
    let denied = b
        .handle(
            request(
                "room-b",
                "control",
                json!({"type":"control","mode":"acquire"}),
            ),
            false,
        )
        .await;
    assert!(!denied.accepted);
    drop(a);
    let restored = DesktopHost::new(fixture, root.path().join("a")).unwrap().with_application_preferences(Some(enabled_preferences(root.path()))).unwrap();
    let denied = restored
        .handle(
            request(
                "room-a",
                "restart",
                json!({"type":"control","mode":"acquire"}),
            ),
            false,
        )
        .await;
    assert!(!denied.accepted);
}

#[test]
fn bare_success_and_mismatched_evidence_do_not_prove_a_desktop_action() {
    let request = request("room", "request", json!({"type":"status"}));
    let response:Response=serde_json::from_value(json!({"schema_version":2,"worker_environment_id":"room","request_id":"request","operation":"status","accepted":true,"evidence":null,"error":null})).unwrap();
    assert!(protocol::validate_response(&response, &request)
        .unwrap_err()
        .contains("without evidence"));
    let mut wrong = response;
    wrong.worker_environment_id = "other".into();
    assert!(protocol::validate_response(&wrong, &request)
        .unwrap_err()
        .contains("identity"));
    let screen: Screen = serde_json::from_value(screen()).unwrap();
    assert_eq!(
        protocol::screen_point(&screen, 500.0, 300.0).unwrap(),
        json!({"x":-250.0,"y":150.0})
    );
    assert!(protocol::screen_point(&screen, 1000.0, 0.0).is_err());
    assert!(protocol::screen_point(&screen, -1.0, 0.0).is_err());
}
