use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::Path,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Instant,
};
use transport::{Listener, ManagerPaths};

mod core;

const REMOTE_TIME: &str = "2026-01-12T08:09:10.123+02:00";
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Adapter {
    value: Mutex<Value>,
    fail_stage: Mutex<Option<PathBuf>>,
    concurrent_edit: Mutex<Option<Value>>,
    applies: AtomicUsize,
}

impl Adapter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            value: Mutex::new(json!({"theme": "system", "nested": {"z": 1, "a": [true]}})),
            fail_stage: Mutex::new(None),
            concurrent_edit: Mutex::new(None),
            applies: AtomicUsize::new(0),
        })
    }
}

impl ConfigAdapter for Adapter {
    fn read(&self) -> Result<Value> {
        Ok(self.value.lock().unwrap().clone())
    }

    fn apply(&self, value: Value, expected: &str) -> Result<()> {
        let mut current = self.value.lock().unwrap();
        if let Some(concurrent) = self.concurrent_edit.lock().unwrap().take() {
            *current = concurrent;
        }
        if digest(&current)? != expected {
            return Err(err("SYNC_LOCAL_CHANGED"));
        }
        *current = value;
        self.applies.fetch_add(1, Ordering::Relaxed);
        if let Some(path) = self.fail_stage.lock().unwrap().take() {
            fs::create_dir(path).unwrap();
        }
        Ok(())
    }
}

fn options(root: &Path) -> SyncOptions {
    SyncOptions {
        app_id: "test-app".into(),
        app_name: "Test Application".into(),
        config_id: "preferences".into(),
        label: "Preferences".into(),
        state_dir: root.join("app"),
    }
}

fn paths(root: &Path) -> ManagerPaths {
    ManagerPaths {
        socket: root.join("manager.sock"),
        token: root.join("manager.token"),
    }
}

fn start_test(root: &Path, adapter: Arc<Adapter>) -> SyncHandle {
    start_at(
        options(root),
        adapter,
        Some(paths(root)),
        Duration::from_millis(50),
    )
    .unwrap()
}

fn eventually(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !check() {
        assert!(Instant::now() < deadline, "condition did not become true");
        thread::sleep(Duration::from_millis(5));
    }
}

fn stop(handle: SyncHandle) {
    let shared = Arc::downgrade(&handle.0.shared);
    let socket = handle.0.shared.socket.clone();
    drop(handle);
    eventually(|| shared.strong_count() == 0 && !socket.exists());
}

fn raw(socket: &Path, request: &[u8]) -> (u16, Value) {
    let mut stream = transport::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream.write_all(request).unwrap();
    let mut response = Vec::new();
    match stream.read_to_end(&mut response) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        Err(error) => panic!("test socket read failed: {error}"),
    }
    // Windows can reset a rejected request whose oversized input remains unread.
    if response.is_empty() {
        return (0, Value::Null);
    }
    let Some(end) = response.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
        return (0, Value::Null);
    };
    let headers = std::str::from_utf8(&response[..end]).unwrap();
    assert!(headers.contains("Cache-Control: no-store"));
    let status = headers.split(' ').nth(1).unwrap().parse().unwrap();
    let Ok(body) = serde_json::from_slice(&response[end + 4..]) else {
        return (0, Value::Null);
    };
    (status, body)
}

fn http(socket: &Path, method: &str, body: Option<Value>) -> (u16, Value) {
    let body = body
        .map(|body| serde_json::to_vec(&body).unwrap())
        .unwrap_or_default();
    let mut request = format!(
        "{method} /config HTTP/1.1\r\nHost: local\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(&body);
    raw(socket, &request)
}

fn get(handle: &SyncHandle) -> Value {
    let (status, value) = http(&handle.0.shared.socket, "GET", None);
    assert_eq!(status, 200);
    value
}

fn put(handle: &SyncHandle, value: &Value, expected: &str, timestamp: &str) -> (u16, Value) {
    http(
        &handle.0.shared.socket,
        "PUT",
        Some(json!({
            "content": STANDARD.encode(serde_json::to_vec(value).unwrap()),
            "updated_at": timestamp,
            "expected_sha256": expected,
        })),
    )
}

#[test]
fn real_callback_canonical_json_conflicts_and_restart_preserve_remote_time() {
    let root = tempfile::tempdir().unwrap();
    let adapter = Adapter::new();
    let handle = start_test(root.path(), adapter.clone());
    let first = get(&handle);
    assert_eq!(first["updated_at"], state::EPOCH);
    let bytes = STANDARD.decode(first["content"].as_str().unwrap()).unwrap();
    assert_eq!(bytes, br#"{"nested":{"a":[true],"z":1},"theme":"system"}"#);
    let original = hash(&bytes);
    let remote = json!({"theme": "dark", "nested": {"a": [false], "z": 4}});
    assert_eq!(put(&handle, &remote, &original, REMOTE_TIME).0, 200);
    assert_eq!(adapter.read().unwrap(), remote);
    handle.local_changed().unwrap();
    assert_eq!(get(&handle)["updated_at"], REMOTE_TIME);
    assert_eq!(
        put(&handle, &json!({"bad": true}), &original, REMOTE_TIME).0,
        409
    );
    assert_eq!(adapter.applies.load(Ordering::Relaxed), 1);
    stop(handle);

    let handle = start_test(root.path(), adapter.clone());
    assert_eq!(get(&handle)["updated_at"], REMOTE_TIME);
    handle.local_changed().unwrap();
    assert_eq!(get(&handle)["updated_at"], REMOTE_TIME);
    *adapter.value.lock().unwrap() = json!({"theme": "light"});
    handle.local_changed().unwrap();
    let local_time = get(&handle)["updated_at"].as_str().unwrap().to_owned();
    assert!(
        chrono::DateTime::parse_from_rfc3339(&local_time).unwrap()
            > chrono::DateTime::parse_from_rfc3339(REMOTE_TIME).unwrap()
    );
    handle.local_changed().unwrap();
    assert_eq!(get(&handle)["updated_at"], local_time);
    stop(handle);
}

#[test]
fn durable_pending_stamp_recovers_after_post_apply_metadata_failure() {
    let root = tempfile::tempdir().unwrap();
    let adapter = Adapter::new();
    let handle = start_test(root.path(), adapter.clone());
    let first = get(&handle);
    let metadata = fs::read_dir(root.path().join("app"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("version.json");
    let stage = metadata.with_extension("stage");
    *adapter.fail_stage.lock().unwrap() = Some(stage.clone());
    let remote = json!({"theme": "remote-import"});
    let expected = hash(&STANDARD.decode(first["content"].as_str().unwrap()).unwrap());
    assert_eq!(put(&handle, &remote, &expected, REMOTE_TIME).0, 500);
    assert_eq!(adapter.read().unwrap(), remote);
    let journal: Value = serde_json::from_slice(&fs::read(&metadata).unwrap()).unwrap();
    assert_eq!(journal["pending"]["updated_at"], REMOTE_TIME);
    assert!(!String::from_utf8(fs::read(&metadata).unwrap())
        .unwrap()
        .contains("remote-import"));
    stop(handle);
    fs::remove_dir(stage).unwrap();
    let handle = start_test(root.path(), adapter.clone());
    assert_eq!(get(&handle)["updated_at"], REMOTE_TIME);
    handle.local_changed().unwrap();
    assert_eq!(get(&handle)["updated_at"], REMOTE_TIME);
    assert_eq!(adapter.applies.load(Ordering::Relaxed), 1);
    stop(handle);
}

#[test]
fn adapter_cas_catches_edit_between_bridge_snapshot_and_apply() {
    let root = tempfile::tempdir().unwrap();
    let adapter = Adapter::new();
    let handle = start_test(root.path(), adapter.clone());
    let expected = digest(&adapter.read().unwrap()).unwrap();
    *adapter.concurrent_edit.lock().unwrap() = Some(json!({"theme": "user-edit"}));
    assert_eq!(
        put(&handle, &json!({"theme": "remote"}), &expected, REMOTE_TIME).0,
        409
    );
    assert_eq!(adapter.read().unwrap(), json!({"theme": "user-edit"}));
    assert_ne!(get(&handle)["updated_at"], REMOTE_TIME);
    stop(handle);
}

struct MockManager {
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
    registrations: Arc<AtomicUsize>,
    operations: Arc<Mutex<Vec<Value>>>,
}

impl MockManager {
    fn start(root: &Path) -> Self {
        let paths = paths(root);
        fs::write(&paths.token, TOKEN).unwrap();
        let listener = Listener::bind(&paths.socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let registrations = Arc::new(AtomicUsize::new(0));
        let operations = Arc::new(Mutex::new(Vec::new()));
        let stopped = stop.clone();
        let registered = registrations.clone();
        let recorded = operations.clone();
        let server = thread::spawn(move || {
            let mut view = json!({
                "items": [{
                    "id": "app:test-app:preferences", "kind": "application",
                    "enabled": false, "auto_sync": false, "label": "Old label",
                    "application": {
                        "app_id": "test-app", "app_name": "Old name", "config_id": "preferences",
                        "sync_interface": {"socket_path": "old.sock", "path": "/config"},
                    },
                }],
                "policy": {"auto_sync_enabled": false},
                "status": {"last_synced_at": REMOTE_TIME, "state": "success", "message": "private-message"},
                "repo": "private-repository", "token": "never-expose-this", "running": false,
                "records": [{"app_id": "test-app", "state": "success", "completed_at": REMOTE_TIME}],
                "deferred": [],
            });
            while !stopped.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("mock manager accept failed: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                if BufReader::new(&mut stream)
                    .read_line(&mut line)
                    .unwrap_or(0)
                    == 0
                {
                    continue;
                }
                let request: Value = serde_json::from_str(&line).unwrap();
                let reply = if request["version"] != 1 || request["token"] != TOKEN {
                    json!({"ok": false, "error": {"code": "UNAUTHORIZED", "message": "private-token"}})
                } else {
                    let op = &request["op"];
                    recorded.lock().unwrap().push(op.clone());
                    match op["op"].as_str().unwrap() {
                        "sync_read" => {}
                        "sync_register_application" => {
                            registered.fetch_add(1, Ordering::Relaxed);
                            let registration = &op["request"];
                            let item = &mut view["items"][0];
                            item["enabled"] = registration["enabled"].clone();
                            item["label"] = registration["label"].clone();
                            for field in ["app_id", "app_name", "config_id", "sync_interface"] {
                                item["application"][field] = registration[field].clone();
                            }
                        }
                        "sync_set_item_enabled" => {
                            assert_eq!(op["id"], "app:test-app:preferences");
                            view["items"][0]["enabled"] = op["enabled"].clone();
                        }
                        "sync_set_application_auto_sync" => {
                            assert_eq!(op["app_id"], "test-app");
                            view["items"][0]["auto_sync"] = op["enabled"].clone();
                        }
                        "sync_run_application" => {
                            assert_eq!(op["app_id"], "test-app");
                            let socket = Path::new(
                                view["items"][0]["application"]["sync_interface"]["socket_path"]
                                    .as_str()
                                    .unwrap(),
                            );
                            let (status, fetched) = http(socket, "GET", None);
                            assert_eq!(status, 200);
                            let expected = hash(
                                &STANDARD
                                    .decode(fetched["content"].as_str().unwrap())
                                    .unwrap(),
                            );
                            assert_eq!(
                                http(
                                    socket,
                                    "PUT",
                                    Some(json!({
                                        "content": STANDARD.encode(br#"{"theme":"from-manager"}"#),
                                        "expected_sha256": expected, "updated_at": REMOTE_TIME,
                                    }))
                                )
                                .0,
                                200
                            );
                        }
                        _ => panic!("unexpected operation"),
                    }
                    json!({"ok": true, "data": view})
                };
                let mut reply = serde_json::to_vec(&reply).unwrap();
                reply.push(b'\n');
                let _ = stream.write_all(&reply);
            }
        });
        Self {
            stop,
            server: Some(server),
            registrations,
            operations,
        }
    }
}

impl Drop for MockManager {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.server.take().unwrap().join().unwrap();
    }
}

#[test]
fn offline_start_retries_registration_preserves_choices_and_manual_callback_cannot_deadlock() {
    let root = tempfile::tempdir().unwrap();
    let adapter = Adapter::new();
    let handle = start_test(root.path(), adapter.clone());
    assert_eq!(handle.status().unwrap()["error_code"], "AGENT_UNAVAILABLE");
    let manager = MockManager::start(root.path());
    // Recovery must happen in the worker without a UI status/control call.
    eventually(|| handle.0.shared.cached_status().unwrap()["registered"] == true);
    let status = handle.status().unwrap();
    assert_eq!(
        status,
        json!({
            "registered": true, "enabled": false, "auto_sync": false,
            "manager_auto_sync": false, "last_synced_at": REMOTE_TIME,
            "error_code": null, "running": false,
        })
    );
    assert_eq!(handle.status().unwrap(), status);
    assert_eq!(manager.registrations.load(Ordering::Relaxed), 1);
    assert!(handle.set_enabled(true).unwrap()["enabled"]
        .as_bool()
        .unwrap());
    assert!(handle.set_auto_sync(true).unwrap()["auto_sync"]
        .as_bool()
        .unwrap());
    assert!(handle.sync_now().unwrap()["error_code"].is_null());
    assert_eq!(adapter.read().unwrap(), json!({"theme": "from-manager"}));
    let output = handle.status().unwrap().to_string();
    for private in [
        "private-repository",
        "private-message",
        "never-expose-this",
        TOKEN,
        "/config",
    ] {
        assert!(!output.contains(private));
    }
    assert!(manager
        .operations
        .lock()
        .unwrap()
        .iter()
        .any(|op| op["op"] == "sync_run_application"));

    fs::write(paths(root.path()).token, "f".repeat(64)).unwrap();
    eventually(|| handle.0.shared.cached_status().unwrap()["error_code"] == "UNAUTHORIZED");
    stop(handle);
}

#[test]
fn framing_limits_reject_ambiguous_or_oversized_requests_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let adapter = Adapter::new();
    let handle = start_test(root.path(), adapter.clone());
    for (headers, expected) in [
        ("Content-Length: 0\r\ncontent-length: 0\r\n".into(), 400),
        ("Transfer-Encoding: chunked\r\n".into(), 400),
        (format!("Content-Length: {}\r\n", 2 * MAX_CONTENT + 1), 413),
        (format!("X-Large: {}\r\n", "x".repeat(17 * 1024)), 400),
    ] {
        let status = raw(
            &handle.0.shared.socket,
            format!("PUT /config HTTP/1.1\r\n{headers}\r\n").as_bytes(),
        )
        .0;
        assert!(status == expected || (headers.starts_with("X-Large:") && status == 0));
    }
    assert_eq!(
        put(
            &handle,
            &json!({"private": "never-in-output"}),
            &"a".repeat(64),
            "invalid"
        )
        .0,
        500
    );
    let oversized = json!({"content": STANDARD.encode(vec![b'x'; MAX_CONTENT + 1]),
        "updated_at": REMOTE_TIME, "expected_sha256": "a".repeat(64)});
    assert_eq!(http(&handle.0.shared.socket, "PUT", Some(oversized)).0, 413);
    assert_eq!(adapter.applies.load(Ordering::Relaxed), 0);
    stop(handle);
}

#[test]
fn instance_lock_keeps_active_socket_and_last_handle_cancels_stalled_manager() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    fs::write(&paths.token, TOKEN).unwrap();
    let listener = Listener::bind(&paths.socket).unwrap();
    let adapter = Adapter::new();
    let handle = start_test(root.path(), adapter.clone());
    let (mut stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut line = String::new();
    BufReader::new(&mut stream).read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["op"]["op"],
        "sync_read"
    );
    let duplicate = start_at(options(root.path()), adapter, Some(paths), OBSERVE_INTERVAL);
    assert_eq!(duplicate.err().unwrap(), "SYNC_BUSY");
    let clone = handle.clone();
    drop(handle);
    assert_eq!(get(&clone)["updated_at"], state::EPOCH);
    let socket = clone.0.shared.socket.clone();
    let before = Instant::now();
    drop(clone);
    assert!(before.elapsed() < Duration::from_millis(100));
    eventually(|| !socket.exists());
    assert_eq!(stream.read(&mut [0u8; 1]).unwrap(), 0);
}

#[test]
fn long_state_path_uses_private_short_socket_and_never_unlinks_another_socket() {
    let root = tempfile::tempdir().unwrap();
    let long = root.path().join("long".repeat(30));
    let adapter = Adapter::new();
    let handle = start_test(&long, adapter);
    let socket = handle.0.shared.socket.clone();
    assert!(socket.to_string_lossy().len() <= 100);
    assert!(!socket.starts_with(&long));
    assert_private(socket.parent().unwrap(), true);
    assert_private(&socket, false);
    let saved = socket.with_extension("old");
    fs::rename(&socket, &saved).unwrap();
    let replacement = Listener::bind(&socket).unwrap();
    let weak = Arc::downgrade(&handle.0.shared);
    drop(handle);
    eventually(|| weak.strong_count() == 0);
    assert!(socket.exists());
    drop(replacement);
    fs::remove_file(socket).unwrap();
    fs::remove_file(saved).unwrap();
}

#[cfg(unix)]
fn assert_private(path: &Path, directory: bool) {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        if directory { 0o700 } else { 0o600 }
    );
}

#[cfg(windows)]
fn assert_private(path: &Path, _: bool) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::*;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let mut needed = 0;
        GetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
        let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        assert_ne!(
            GetFileSecurityW(
                path.as_ptr(),
                DACL_SECURITY_INFORMATION,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed
            ),
            0
        );
        let mut control = 0;
        let mut revision = 0;
        assert_ne!(
            GetSecurityDescriptorControl(buffer.as_mut_ptr().cast(), &mut control, &mut revision),
            0
        );
        assert_ne!(control & SE_DACL_PROTECTED, 0);
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = std::ptr::null_mut();
        assert_ne!(
            GetSecurityDescriptorDacl(
                buffer.as_mut_ptr().cast(),
                &mut present,
                &mut acl,
                &mut defaulted
            ),
            0
        );
        assert_ne!(present, 0);
        assert!(!acl.is_null());
        assert_eq!((*acl).AceCount, 1);
    }
}

#[cfg(unix)]
#[test]
fn state_rejects_symlinked_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    fs::create_dir(&real).unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(
        start_at(options(&link), Adapter::new(), None, OBSERVE_INTERVAL)
            .err()
            .unwrap(),
        "SYNC_CONFIG"
    );
}
