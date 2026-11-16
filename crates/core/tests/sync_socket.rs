use ai_config_core::{
    agent::{self, AgentOp, AgentSyncSettings},
    rest,
    sync::{SyncPolicy, SyncSecrets, SyncStore},
    Result, SecretKey, Store, Vault,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(windows)]
use uds_windows::UnixStream;

#[derive(Default)]
struct Secrets(Mutex<HashMap<String, String>>);
impl SyncSecrets for Secrets {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }
}
struct NoVault;
impl Vault for NoVault {
    fn get_key(&self) -> Result<Option<SecretKey>> {
        panic!("socket sync tests must not read native credentials")
    }
    fn create_key(&self) -> Result<SecretKey> {
        panic!("socket sync tests must not write native credentials")
    }
}

fn http(socket: &Path, token: &str, method: &str, path: &str, body: Value) -> Value {
    http_status(socket, token, method, path, body, 200)
}

fn http_status(
    socket: &Path,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
    status: u16,
) -> Value {
    use std::io::{BufRead, BufReader};
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let body = body.to_string();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: local\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    // Read exactly one response by framing, not by waiting for socket EOF:
    // Windows AF_UNIX does not reliably surface a peer close promptly.
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).unwrap();
        assert!(read > 0, "connection closed before response headers");
        if line == "\r\n" || line == "\n" {
            break;
        }
        headers.push_str(&line);
    }
    let length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
        })
        .expect("Content-Length header");
    let mut payload = vec![0u8; length];
    std::io::Read::read_exact(&mut reader, &mut payload).unwrap();
    let reply = String::from_utf8(payload).unwrap();
    assert!(headers.starts_with(&format!("HTTP/1.1 {status} ")));
    assert!(headers.contains("Cache-Control: no-store"));
    assert!(!reply.contains("synthetic-"));
    serde_json::from_str(&reply).unwrap()
}

#[test]
fn json_and_rest_share_authenticated_sync_registration_and_state() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let socket = root.join("agent.sock");
    let listener = agent::bind(&socket).unwrap();
    let token = agent::new_token();
    let server_token = token.clone();
    let sync_path = root.join("sync.json");
    let store = Arc::new(Store::new(root.join("ai.yml"), Arc::new(NoVault)));
    let sync = Arc::new(SyncStore::new(
        sync_path.clone(),
        store.clone(),
        Arc::new(Secrets::default()),
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server = thread::spawn(move || {
        while !server_stop.load(Ordering::SeqCst) {
            let (stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(_) if server_stop.load(Ordering::SeqCst) => break,
                Err(_) => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
            };
            if server_stop.load(Ordering::SeqCst) {
                break;
            }
            let token = server_token.clone();
            let store = store.clone();
            let sync = Arc::clone(&sync);
            // Serve each connection independently: a slow or abandoned peer
            // must not stall the accept loop and time out later requests.
            thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
                let _ = agent::serve_stream_with_rest(
                    stream,
                    &token,
                    &|op| agent::dispatch(&store, &sync, op),
                    &|request| rest::handle_rest(&store, &sync, &request),
                );
            });
            // Give the accepted handle a moment to detach before accepting
            // again; rapid accept/close cycles are unreliable on Windows AF_UNIX.
            thread::sleep(Duration::from_millis(2));
        }
    });
    let denied = agent::request_at(&socket, &agent::new_token(), AgentOp::SyncRun).unwrap();
    assert_eq!(denied.error.as_ref().unwrap().code, "UNAUTHORIZED");
    let unconfigured = agent::request_at(&socket, &token, AgentOp::SyncRun).unwrap();
    assert_eq!(
        unconfigured.error.as_ref().unwrap().code,
        "SYNC_NOT_CONFIGURED"
    );

    let saved = agent::request_at(
        &socket,
        &token,
        AgentOp::SyncSaveSettings {
            request: AgentSyncSettings {
                provider: Default::default(),
                git_branch: "main".into(),
                repo: "alice/configs".into(),
                github_token: Some("synthetic-github-token".into()),
                encryption_secret: Some("synthetic-sync-secret".into()),
            },
        },
    )
    .unwrap()
    .into_result()
    .unwrap();
    assert_eq!(saved["token_present"], true);
    assert!(!saved.to_string().contains("synthetic-"));

    let file = root.join("third-party.json");
    std::fs::write(&file, r#"{"provider":"example"}"#).unwrap();
    let added = agent::request_at(
        &socket,
        &token,
        AgentOp::SyncAddItem {
            label: Some("Third party".into()),
            path: file.to_str().unwrap().into(),
            enabled: true,
        },
    )
    .unwrap()
    .into_result()
    .unwrap();
    let id = added["items"][1]["id"].as_str().unwrap();
    let updated = http(
        &socket,
        &token,
        "PATCH",
        &format!("/sync/items/{id}"),
        json!({ "enabled": false }),
    );
    assert_eq!(updated["data"]["items"][1]["enabled"], false);
    let read = agent::request_at(&socket, &token, AgentOp::SyncRead)
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(read["items"][1]["enabled"], false);
    let deleted = http(
        &socket,
        &token,
        "DELETE",
        &format!("/sync/items/{id}"),
        json!(null),
    );
    assert_eq!(deleted["data"]["items"].as_array().unwrap().len(), 1);
    let state = http(&socket, &token, "GET", "/sync", json!(null));
    assert_eq!(state["data"]["repo"], "alice/configs");
    assert!(!state.to_string().contains("synthetic-"));
    let registered = http(
        &socket,
        &token,
        "POST",
        "/sync/applications",
        json!({
            "app_id":"editor", "app_name":"Editor", "config_id":"settings", "label":"Editor settings",
            "sync_interface":{"socket_path":root.join("editor.sock"), "path":"/config"}, "enabled":true,
        }),
    );
    assert_eq!(
        registered["data"]["items"][1]["application"]["app_name"],
        "Editor"
    );
    let policy = agent::request_at(
        &socket,
        &token,
        AgentOp::SyncSavePolicy {
            policy: SyncPolicy {
                auto_sync_enabled: true,
                sync_interval_minutes: 10,
                history_retention_days: 0,
            },
        },
    )
    .unwrap()
    .into_result()
    .unwrap();
    assert_eq!(policy["policy"]["sync_interval_minutes"], 10);
    assert!(policy["next_sync_at"].is_string());
    let records = http(&socket, &token, "GET", "/sync/history", json!(null));
    assert_eq!(records["data"][0]["error_code"], "SYNC_NOT_CONFIGURED");
    let cleaned = agent::request_at(&socket, &token, AgentOp::SyncCleanup)
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(cleaned["records"].as_array().unwrap().len(), 1);
    let bare = root.join("remote.git");
    let initialized = std::process::Command::new("git")
        .args(["init", "--bare", "--template="])
        .arg(directory.path().join("remote.git"))
        .output()
        .unwrap();
    assert!(initialized.status.success());
    let probe = http(
        &socket,
        &token,
        "POST",
        "/sync/test",
        json!({ "provider":"git", "repo":bare, "git_branch":"main" }),
    );
    assert_eq!(probe["data"]["ok"], true);
    assert_eq!(probe["data"]["cleanup_ok"], true);
    assert_eq!(probe["data"]["probe_ref"], Value::Null);
    let saved_git = http(
        &socket,
        &token,
        "POST",
        "/sync/settings",
        json!({ "provider":"git", "repo":bare, "git_branch":"settings/device" }),
    );
    assert_eq!(saved_git["data"]["provider"], "git");
    assert_eq!(saved_git["data"]["git_branch"], "settings/device");
    assert_eq!(saved_git["data"]["token_present"], false);
    assert_eq!(saved_git["data"]["encryption_secret_present"], true);
    let deadline = &saved_git["data"]["next_sync_at"];

    let before = std::fs::read(&sync_path).unwrap();
    for op in [
        AgentOp::SyncRunApplication {
            app_id: "editor".into(),
        },
        AgentOp::SyncSetApplicationAutoSync {
            app_id: "editor".into(),
            enabled: false,
        },
    ] {
        let denied = agent::request_at(&socket, &agent::new_token(), op).unwrap();
        assert_eq!(denied.error.as_ref().unwrap().code, "UNAUTHORIZED");
    }
    for (method, suffix, body) in [
        ("POST", "run", json!(null)),
        ("PATCH", "automation", json!({ "enabled": false })),
    ] {
        let denied = http_status(
            &socket,
            &agent::new_token(),
            method,
            &format!("/sync/applications/editor/{suffix}"),
            body,
            401,
        );
        assert_eq!(denied["error"]["code"], "UNAUTHORIZED");
    }
    for (app_id, code, status) in [
        (String::new(), "SYNC_APPLICATION", 400),
        ("bad/id".into(), "SYNC_APPLICATION", 400),
        ("bad%2Fid".into(), "SYNC_APPLICATION", 400),
        ("x".repeat(81), "SYNC_APPLICATION", 400),
        ("missing".into(), "SYNC_ITEM_MISSING", 404),
    ] {
        for op in [
            AgentOp::SyncRunApplication {
                app_id: app_id.clone(),
            },
            AgentOp::SyncSetApplicationAutoSync {
                app_id: app_id.clone(),
                enabled: false,
            },
        ] {
            let rejected = agent::request_at(&socket, &token, op).unwrap();
            assert_eq!(rejected.error.as_ref().unwrap().code, code);
        }
        for (method, suffix, body) in [
            ("POST", "run", json!(null)),
            ("PATCH", "automation", json!({ "enabled": false })),
        ] {
            let rejected = http_status(
                &socket,
                &token,
                method,
                &format!("/sync/applications/{app_id}/{suffix}"),
                body,
                status,
            );
            assert_eq!(rejected["error"]["code"], code);
        }
    }
    for body in [
        json!({}),
        json!({ "enabled": "false" }),
        json!({ "enabled": false, "unknown": true }),
    ] {
        let rejected = http_status(
            &socket,
            &token,
            "PATCH",
            "/sync/applications/editor/automation",
            body,
            400,
        );
        assert_eq!(rejected["error"]["code"], "INVALID_ARGUMENT");
    }
    assert_eq!(std::fs::read(&sync_path).unwrap(), before);

    for (app_id, config_id, enabled) in [
        ("editor", "shortcuts", false),
        ("other-editor", "settings", true),
    ] {
        http(
            &socket,
            &token,
            "POST",
            "/sync/applications",
            json!({
                "app_id": app_id, "app_name": "Editor", "config_id": config_id,
                "label": "Preferences", "enabled": enabled,
                "sync_interface": { "socket_path": root.join("editor.sock"), "path": "/config" },
            }),
        );
    }
    http(
        &socket,
        &token,
        "PATCH",
        "/sync/items/app:editor:settings",
        json!({ "enabled": false }),
    );
    let automatic = http(
        &socket,
        &token,
        "PATCH",
        "/sync/applications/editor/automation",
        json!({ "enabled": false }),
    );
    let shared = agent::request_at(&socket, &token, AgentOp::SyncRead)
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(shared, automatic["data"]);
    assert_eq!(shared["items"][1]["auto_sync"], false);
    assert_eq!(shared["items"][2]["auto_sync"], false);
    assert_eq!(shared["items"][3]["auto_sync"], true);
    let reregistered = http(
        &socket,
        &token,
        "POST",
        "/sync/applications",
        json!({
            "app_id": "editor", "app_name": "Updated editor", "config_id": "settings",
            "label": "Updated settings", "enabled": true,
            "sync_interface": { "socket_path": root.join("updated.sock"), "path": "/config" },
        }),
    );
    assert_eq!(reregistered["data"]["items"][1]["enabled"], false);
    assert_eq!(reregistered["data"]["items"][1]["auto_sync"], false);
    assert_eq!(
        reregistered["data"]["items"][1]["label"],
        "Updated settings"
    );
    assert_eq!(&reregistered["data"]["next_sync_at"], deadline);
    let disabled = agent::request_at(
        &socket,
        &token,
        AgentOp::SyncRunApplication {
            app_id: "editor".into(),
        },
    )
    .unwrap();
    assert_eq!(disabled.error.as_ref().unwrap().code, "SYNC_DISABLED");
    let disabled = http_status(
        &socket,
        &token,
        "POST",
        "/sync/applications/editor/run",
        json!(null),
        400,
    );
    assert_eq!(disabled["error"]["code"], "SYNC_DISABLED");
    let automatic = agent::request_at(
        &socket,
        &token,
        AgentOp::SyncSetApplicationAutoSync {
            app_id: "editor".into(),
            enabled: true,
        },
    )
    .unwrap()
    .into_result()
    .unwrap();
    let shared = http(&socket, &token, "GET", "/sync", json!(null));
    assert_eq!(shared["data"], automatic);
    assert_eq!(automatic["items"][1]["auto_sync"], true);
    assert_eq!(automatic["items"][2]["auto_sync"], true);
    assert_eq!(automatic["items"][1]["enabled"], false);
    assert_eq!(&automatic["next_sync_at"], deadline);
    agent::request_at(
        &socket,
        &token,
        AgentOp::SyncSetApplicationAutoSync {
            app_id: "editor".into(),
            enabled: false,
        },
    )
    .unwrap()
    .into_result()
    .unwrap();
    agent::request_at(
        &socket,
        &token,
        AgentOp::SyncSetItemEnabled {
            id: "app:editor:settings".into(),
            enabled: true,
        },
    )
    .unwrap()
    .into_result()
    .unwrap();
    // Offline callbacks are deferred; scoped manual runs must still leave the
    // unrelated AI item untouched and keep automatic participation disabled.
    let manual = agent::request_at(
        &socket,
        &token,
        AgentOp::SyncRunApplication {
            app_id: "editor".into(),
        },
    )
    .unwrap()
    .into_result()
    .unwrap();
    assert_eq!(manual["records"][0]["app_id"], "editor");
    assert_eq!(manual["records"][0]["trigger"], "agent");
    // Automatic participation is off, so the offline callback is not queued for
    // retry; the manual attempt itself must still report the fixed failure.
    assert_eq!(
        manual["records"][0]["state"], "error",
        "record={}",
        manual["records"][0]
    );
    assert_eq!(
        manual["records"][0]["error_code"],
        "SYNC_APPLICATION_OFFLINE"
    );
    assert_eq!(manual["items"][1]["auto_sync"], false);
    assert_eq!(&manual["next_sync_at"], deadline);
    let manual = http(
        &socket,
        &token,
        "POST",
        "/sync/applications/editor/run",
        json!(null),
    );
    assert_eq!(&manual["data"]["next_sync_at"], deadline);
    let records = http(&socket, &token, "GET", "/sync/history", json!(null));
    assert_eq!(records["data"].as_array().unwrap().len(), 3);
    for record in records["data"].as_array().unwrap().iter().take(2) {
        assert_eq!(record["app_id"], "editor");
        assert_eq!(record["state"], "error");
        assert_eq!(record["error_code"], "SYNC_APPLICATION_OFFLINE");
        assert_eq!(record["trigger"], "agent");
    }
    assert!(records["data"][2]["app_id"].is_null());
    let shared = agent::request_at(&socket, &token, AgentOp::SyncRead)
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(shared, manual["data"]);
    assert_eq!(shared["records"], records["data"]);
    stop.store(true, Ordering::SeqCst);
    // The worker blocks in accept; one throwaway connection lets it observe the
    // stop flag and exit without a timed accept loop.
    let _ = UnixStream::connect(&socket);
    server.join().unwrap();
    let persisted = std::fs::read_to_string(sync_path).unwrap();
    assert!(!persisted.contains("synthetic-"));
    assert!(!root.join("ai.yml").exists());
}
