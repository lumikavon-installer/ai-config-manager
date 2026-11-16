//! Exercise the production manager protocol, merge, encryption and Git backend.
//! Only the credential vault and application settings are test implementations.
use super::*;
use ai_config_core::{
    agent::{self, AgentOp, AgentSyncSettings},
    sync::{SyncPolicy, SyncProvider, SyncSecrets, SyncStore, AI_CONFIG_ITEM_ID, SYNC_REMOTE_PATH},
    SecretKey, Store, Vault,
};
use std::{collections::HashMap, process::Command};

#[derive(Default)]
struct Secrets(Mutex<HashMap<String, String>>);
impl SyncSecrets for Secrets {
    fn get(&self, account: &str) -> ai_config_core::Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> ai_config_core::Result<()> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }
}

struct NoVault;
impl Vault for NoVault {
    fn get_key(&self) -> ai_config_core::Result<Option<SecretKey>> {
        panic!("application sync must not access the AI credential vault")
    }
    fn create_key(&self) -> ai_config_core::Result<SecretKey> {
        panic!("application sync must not create an AI credential")
    }
}

struct Manager {
    paths: ManagerPaths,
    path: PathBuf,
    store: Arc<Store>,
    secrets: Arc<Secrets>,
    sync: Arc<Mutex<SyncStore>>,
    stopped: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}

impl Manager {
    fn new(root: &Path) -> Self {
        platform::private_dir(root).unwrap();
        let (listener, socket_guard) = transport::bind(root).unwrap();
        let paths = ManagerPaths {
            socket: socket_guard.path.clone(),
            token: root.join("agent.token"),
        };
        platform::atomic_write(&paths.token, TOKEN.as_bytes()).unwrap();
        let path = root.join("sync.json");
        let store = Arc::new(Store::new(root.join("ai.yml"), Arc::new(NoVault)));
        let secrets = Arc::new(Secrets::default());
        let sync = SyncStore::new(path.clone(), store.clone(), secrets.clone());
        sync.set_item_enabled(AI_CONFIG_ITEM_ID, false).unwrap();
        let sync = Arc::new(Mutex::new(sync));
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stop = stopped.clone();
        let worker_sync = sync.clone();
        let worker_store = store.clone();
        let server = thread::spawn(move || {
            let _socket_guard = socket_guard;
            while !worker_stop.load(Ordering::Acquire) {
                let (stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("manager accept: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                // Last-handle shutdown may cancel an observer's socket read.
                let _ = agent::serve_stream(stream, TOKEN, &|op| {
                    agent::dispatch(&worker_store, &worker_sync.lock().unwrap(), op)
                });
            }
        });
        Self {
            paths,
            path,
            store,
            secrets,
            sync,
            stopped,
            server: Some(server),
        }
    }

    fn request(&self, op: AgentOp) -> Value {
        agent::request_at(&self.paths.socket, TOKEN, op)
            .unwrap()
            .into_result()
            .unwrap()
    }

    fn configure(&self, remote: &Path) {
        self.request(AgentOp::SyncSaveSettings {
            request: AgentSyncSettings {
                provider: SyncProvider::Git,
                git_branch: "main".into(),
                repo: remote.to_str().unwrap().into(),
                github_token: None,
                encryption_secret: Some("synthetic-integration-secret".into()),
            },
        });
    }

    fn app(&self, directory: &Path, id: &str, adapter: Arc<dyn ConfigAdapter>) -> SyncHandle {
        let mut options = options(directory);
        options.app_id = id.into();
        let handle =
            start_at(options, adapter, Some(self.paths.clone()), OBSERVE_INTERVAL).unwrap();
        assert_eq!(handle.status().unwrap()["registered"], true);
        handle
    }

    fn view(&self) -> Value {
        self.request(AgentOp::SyncRead)
    }

    fn policy(&self, enabled: bool) {
        self.request(AgentOp::SyncSavePolicy {
            policy: SyncPolicy {
                auto_sync_enabled: enabled,
                sync_interval_minutes: 10,
                history_retention_days: 0,
            },
        });
    }

    fn scheduled(&self) -> Option<Value> {
        // Advance only this isolated manager's persisted deadline, then reload
        // exactly as startup does. No clock or home-directory environment hooks.
        let mut sync = self.sync.lock().unwrap();
        let mut saved: Value = serde_json::from_slice(&fs::read(&self.path).unwrap()).unwrap();
        saved["next_sync_at"] = json!(state::EPOCH);
        fs::write(&self.path, serde_json::to_vec(&saved).unwrap()).unwrap();
        *sync = SyncStore::new(self.path.clone(), self.store.clone(), self.secrets.clone());
        sync.run_scheduled()
            .unwrap()
            .map(|view| serde_json::to_value(view).unwrap())
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.server.take().unwrap().join().unwrap();
    }
}

struct Settings {
    path: PathBuf,
    mutation: Mutex<()>,
    concurrent_edit: Mutex<Option<Value>>,
    applies: AtomicUsize,
}

impl Settings {
    fn new(root: &Path, local: &str) -> Arc<Self> {
        fs::create_dir_all(root).unwrap();
        let path = root.join("settings.json");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "portable": {"theme": "system"}, "local_only": local,
            }))
            .unwrap(),
        )
        .unwrap();
        Arc::new(Self {
            path,
            mutation: Mutex::new(()),
            concurrent_edit: Mutex::new(None),
            applies: AtomicUsize::new(0),
        })
    }

    fn settings(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.path).unwrap()).unwrap()
    }

    fn save(&self, portable: Value) {
        let _lock = self.mutation.lock().unwrap();
        self.save_locked(portable);
    }

    fn save_locked(&self, portable: Value) {
        let mut settings = self.settings();
        settings["portable"] = portable;
        fs::write(&self.path, serde_json::to_vec(&settings).unwrap()).unwrap();
    }
}

impl ConfigAdapter for Settings {
    fn read(&self) -> Result<Value> {
        let _lock = self.mutation.lock().unwrap();
        Ok(self.settings()["portable"].clone())
    }
    fn apply(&self, value: Value, expected: &str) -> Result<()> {
        let _lock = self.mutation.lock().unwrap();
        if let Some(edit) = self.concurrent_edit.lock().unwrap().take() {
            self.save_locked(edit);
        }
        if digest(&self.settings()["portable"])? != expected {
            return Err(err("SYNC_LOCAL_CHANGED"));
        }
        self.save_locked(value);
        self.applies.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn git(remote: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(remote)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "test Git command failed: {args:?}");
    output.stdout
}

fn bare(root: &Path) -> PathBuf {
    let remote = root.join("remote.git");
    let output = Command::new("git")
        .args(["init", "--bare", "--template="])
        .arg(&remote)
        .output()
        .unwrap();
    assert!(output.status.success());
    remote
}

fn head(remote: &Path) -> Vec<u8> {
    git(remote, &["rev-parse", "refs/heads/main"])
}

fn record(view: &Value, uploaded: usize, downloaded: usize) {
    assert_eq!(view["records"][0]["state"], "success", "{view}");
    assert_eq!(view["records"][0]["uploaded_count"], uploaded);
    assert_eq!(view["records"][0]["downloaded_count"], downloaded);
}

#[test]
fn git_roundtrip_manager_and_application_manual_sync_auto_selection_and_no_echo() {
    let root = tempfile::tempdir().unwrap();
    let remote = bare(root.path());
    let first = Manager::new(&root.path().join("manager-a"));
    let second = Manager::new(&root.path().join("manager-b"));
    first.configure(&remote);
    second.configure(&remote);
    let a_dir = root.path().join("device-a");
    let b_dir = root.path().join("device-b");
    let a = Settings::new(&a_dir, "device-a-only");
    let b = Settings::new(&b_dir, "device-b-only");
    let a_handle = first.app(&a_dir, "editor", a.clone());
    let mut b_handle = second.app(&b_dir, "editor", b.clone());
    assert_eq!(get(&b_handle)["updated_at"], state::EPOCH);
    a.save(json!({"theme":"dark", "nested":{"z":1,"a":[true]}}));
    a_handle.local_changed().unwrap();
    let original_stamp = get(&a_handle)["updated_at"].clone();
    let deadline = first.view()["next_sync_at"].clone();
    assert!(a_handle.sync_now().unwrap()["error_code"].is_null());
    let view = first.view();
    record(&view, 1, 0);
    assert_eq!(view["records"][0]["app_id"], "editor");
    assert_eq!(view["next_sync_at"], deadline);
    let initial_head = head(&remote);
    let encrypted = git(&remote, &["show", &format!("main:{SYNC_REMOTE_PATH}")]);
    let encrypted = String::from_utf8(encrypted).unwrap();
    for private in ["theme", "device-a-only", "synthetic-integration-secret"] {
        assert!(!encrypted.contains(private));
    }

    // The manager's own Sync now uses the real callback to import remote data.
    record(&second.request(AgentOp::SyncRun), 0, 1);
    assert_eq!(a.read().unwrap(), b.read().unwrap());
    assert_eq!(b.settings()["local_only"], "device-b-only");
    assert_eq!(get(&b_handle)["updated_at"], original_stamp);
    b_handle.local_changed().unwrap();
    assert!(b_handle.sync_now().unwrap()["error_code"].is_null());
    record(&second.view(), 0, 0);
    assert_eq!(head(&remote), initial_head);
    stop(b_handle);
    b_handle = second.app(&b_dir, "editor", b.clone());
    assert_eq!(get(&b_handle)["updated_at"], original_stamp);
    b_handle.sync_now().unwrap();
    assert_eq!(head(&remote), initial_head);

    assert_eq!(b_handle.set_auto_sync(false).unwrap()["auto_sync"], false);
    // Another app remains eligible, proving selection actually excludes editor.
    let other_dir = root.path().join("other");
    let other = Settings::new(&other_dir, "other-only");
    let other_handle = second.app(&other_dir, "other", other.clone());
    other.save(json!({"theme":"other-v1"}));
    other_handle.local_changed().unwrap();
    b.save(json!({"theme":"light"}));
    b_handle.local_changed().unwrap();
    let changed_stamp = get(&b_handle)["updated_at"].clone();
    assert!(
        chrono::DateTime::parse_from_rfc3339(changed_stamp.as_str().unwrap()).unwrap()
            > chrono::DateTime::parse_from_rfc3339(original_stamp.as_str().unwrap()).unwrap()
    );
    let editor_completed_at = b_handle.status().unwrap()["last_synced_at"].clone();
    record(&second.scheduled().unwrap(), 1, 0);
    assert_eq!(
        b_handle.status().unwrap()["last_synced_at"],
        editor_completed_at
    );
    a_handle.sync_now().unwrap();
    assert_eq!(a.read().unwrap()["theme"], "dark");
    // Manual application sync still works with automatic participation off.
    b_handle.sync_now().unwrap();
    record(&second.view(), 1, 0);
    first.request(AgentOp::SyncRun);
    assert_eq!(a.read().unwrap()["theme"], "light");
    assert_eq!(get(&a_handle)["updated_at"], changed_stamp);
    assert_eq!(a.settings()["local_only"], "device-a-only");

    // A third instance of the other app proves scoped writes retain its bundle.
    let mirror_dir = root.path().join("other-mirror");
    let mirror = Settings::new(&mirror_dir, "mirror-only");
    let mirror_handle = first.app(&mirror_dir, "other", mirror.clone());
    mirror_handle.sync_now().unwrap();
    assert_eq!(mirror.read().unwrap(), other.read().unwrap());

    b_handle.set_auto_sync(true).unwrap();
    second.policy(false);
    b.save(json!({"theme":"scheduled-v2"}));
    b_handle.local_changed().unwrap();
    let before_disabled = head(&remote);
    assert!(second.scheduled().is_none());
    assert_eq!(head(&remote), before_disabled);
    second.policy(true);
    record(&second.scheduled().unwrap(), 1, 0);
    first.request(AgentOp::SyncRun);
    assert_eq!(a.read().unwrap()["theme"], "scheduled-v2");

    // Manager-owned choices survive callback replacement and re-registration.
    b_handle.set_enabled(false).unwrap();
    b_handle.set_auto_sync(false).unwrap();
    assert_eq!(b_handle.sync_now().unwrap_err(), "SYNC_DISABLED");
    stop(b_handle);
    b_handle = second.app(&b_dir, "editor", b.clone());
    assert_eq!(b_handle.status().unwrap()["enabled"], false);
    assert_eq!(b_handle.status().unwrap()["auto_sync"], false);
    b.save(json!({"theme":"disabled-local"}));
    b_handle.local_changed().unwrap();
    let before_disabled = head(&remote);
    record(&second.scheduled().unwrap(), 0, 0);
    assert_eq!(head(&remote), before_disabled);
    b_handle.set_enabled(true).unwrap();
    b_handle.sync_now().unwrap();
    record(&second.view(), 1, 0);

    for manager in [&first, &second] {
        let state = fs::read_to_string(&manager.path).unwrap();
        assert!(!state.contains("synthetic-integration-secret"));
        assert!(!manager.path.parent().unwrap().join("ai.yml").exists());
    }
    stop(mirror_handle);
    stop(other_handle);
    stop(b_handle);
    stop(a_handle);
}

#[test]
fn real_manager_callback_cas_preserves_concurrent_save_and_new_version_uploads() {
    let root = tempfile::tempdir().unwrap();
    let remote = bare(root.path());
    let first = Manager::new(&root.path().join("manager-a"));
    let second = Manager::new(&root.path().join("manager-b"));
    first.configure(&remote);
    second.configure(&remote);
    let a_dir = root.path().join("device-a");
    let b_dir = root.path().join("device-b");
    let a = Settings::new(&a_dir, "a-local");
    let b = Settings::new(&b_dir, "b-local");
    let a_handle = first.app(&a_dir, "editor", a.clone());
    let b_handle = second.app(&b_dir, "editor", b.clone());
    a.save(json!({"theme":"remote-winner"}));
    a_handle.local_changed().unwrap();
    a_handle.sync_now().unwrap();
    let remote_head = head(&remote);
    let remote_stamp = get(&a_handle)["updated_at"].clone();
    *b.concurrent_edit.lock().unwrap() = Some(json!({"theme":"concurrent-save"}));
    assert_eq!(
        b_handle.sync_now().unwrap()["error_code"],
        "SYNC_LOCAL_CHANGED"
    );
    let view = second.view();
    assert_eq!(view["deferred"][0]["reason"], "SYNC_LOCAL_CHANGED");
    record(&view, 0, 0);
    assert_eq!(b.applies.load(Ordering::Relaxed), 0);
    assert_eq!(b.read().unwrap()["theme"], "concurrent-save");
    assert_eq!(b.settings()["local_only"], "b-local");
    assert_eq!(head(&remote), remote_head);
    b_handle.local_changed().unwrap();
    assert_ne!(get(&b_handle)["updated_at"], remote_stamp);
    b_handle.sync_now().unwrap();
    record(&second.view(), 1, 0);
    assert!(second.view()["deferred"].as_array().unwrap().is_empty());
    a_handle.sync_now().unwrap();
    assert_eq!(a.read().unwrap()["theme"], "concurrent-save");
    stop(b_handle);
    stop(a_handle);
}

#[test]
fn application_results_ignore_other_app_failures_but_manager_busy_is_global() {
    let root = tempfile::tempdir().unwrap();
    let manager = Manager::new(&root.path().join("manager"));
    let a_dir = root.path().join("a");
    let b_dir = root.path().join("b");
    let a = manager.app(&a_dir, "first", Settings::new(&a_dir, "a-only"));
    let b = manager.app(&b_dir, "second", Settings::new(&b_dir, "b-only"));
    assert_eq!(b.sync_now().unwrap_err(), "SYNC_NOT_CONFIGURED");
    assert_eq!(b.status().unwrap()["error_code"], "SYNC_NOT_CONFIGURED");
    assert!(a.status().unwrap()["error_code"].is_null());
    assert!(a.status().unwrap()["last_synced_at"].is_null());
    let remote = bare(root.path());
    manager.configure(&remote);
    a.sync_now().unwrap();
    let succeeded_at = a.status().unwrap()["last_synced_at"].clone();
    assert!(succeeded_at.is_string());

    // Empty only the synthetic vault to fail the other application's real run.
    manager.secrets.0.lock().unwrap().clear();
    assert_eq!(b.sync_now().unwrap_err(), "SYNC_NOT_CONFIGURED");
    let status = a.status().unwrap();
    assert!(status["error_code"].is_null());
    assert_eq!(status["last_synced_at"], succeeded_at);
    let mut view = manager.view();
    view["running"] = json!(true);
    assert_eq!(a.0.shared.accept_view(&view).unwrap()["running"], true);
    // A manager-wide failure remains relevant to both enabled applications.
    let reply = agent::request_at(&manager.paths.socket, TOKEN, AgentOp::SyncRun).unwrap();
    assert_eq!(reply.error.as_ref().unwrap().code, "SYNC_NOT_CONFIGURED");
    assert_eq!(a.status().unwrap()["error_code"], "SYNC_NOT_CONFIGURED");
    assert_eq!(a.status().unwrap()["last_synced_at"], succeeded_at);
    let mut unknown_error = manager.view();
    unknown_error["records"][0]["error_code"] = json!("private-os-error-and-path");
    assert_eq!(
        a.0.shared.accept_view(&unknown_error).unwrap()["error_code"],
        "SYNC_FAILED"
    );
    // Pruned records do not resurrect an unrelated global failure. Managers
    // predating the records field retain the documented global fallback.
    unknown_error["records"] = json!([]);
    let pruned = a.0.shared.accept_view(&unknown_error).unwrap();
    assert!(pruned["error_code"].is_null());
    assert!(pruned["last_synced_at"].is_null());
    unknown_error.as_object_mut().unwrap().remove("records");
    let legacy = a.0.shared.accept_view(&unknown_error).unwrap();
    assert_eq!(legacy["error_code"], "SYNC_FAILED");
    assert!(legacy["last_synced_at"].is_string());
    stop(b);
    stop(a);
}
