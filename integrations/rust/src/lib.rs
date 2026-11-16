//! Synchronize an application's portable JSON preferences with AI Config Manager.
//! See the crate README for lifecycle, adapter locking, and status semantics.

mod platform;
mod state;
mod transport;

use serde::{ser::SerializeMap, ser::SerializeSeq, Serialize, Serializer};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use zeroize::{Zeroize, Zeroizing};

pub type Result<T> = std::result::Result<T, String>;

/// Export only portable preferences. `apply` must compare the current digest
/// with `expected_sha256` under the same mutation lock as ordinary user saves.
pub trait ConfigAdapter: Send + Sync + 'static {
    fn read(&self) -> Result<Value>;
    fn apply(&self, value: Value, expected_sha256: &str) -> Result<()>;
}

pub struct SyncOptions {
    pub app_id: String,
    pub app_name: String,
    pub config_id: String,
    pub label: String,
    pub state_dir: PathBuf,
}

/// Cloneable process-lifetime owner. Dropping the last handle cancels workers.
#[derive(Clone)]
pub struct SyncHandle(Arc<Owner>);

struct Owner {
    shared: Arc<Shared>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.shared.control.stop();
        // No join on application code or a potentially long manager operation.
        // Socket I/O is cancelled, and idle workers wake immediately.
    }
}

struct Shared {
    options: SyncOptions,
    adapter: Arc<dyn ConfigAdapter>,
    state: Mutex<state::State>,
    status: Mutex<Value>,
    registration: Mutex<()>,
    socket: PathBuf,
    manager: Option<transport::ManagerPaths>,
    control: Arc<transport::Control>,
    _lease: File,
}

const MAX_CONTENT: usize = 1024 * 1024;
const OBSERVE_INTERVAL: Duration = Duration::from_secs(30);

/// Start the private callback and retry worker. Manager availability is nonfatal.
pub fn start(options: SyncOptions, adapter: Arc<dyn ConfigAdapter>) -> Result<SyncHandle> {
    start_at(
        options,
        adapter,
        transport::manager_paths().ok(),
        OBSERVE_INTERVAL,
    )
}

fn start_at(
    mut options: SyncOptions,
    adapter: Arc<dyn ConfigAdapter>,
    manager: Option<transport::ManagerPaths>,
    observe_interval: Duration,
) -> Result<SyncHandle> {
    fn id(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 80
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    }
    fn label(value: &str) -> bool {
        !value.trim().is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
    }
    if !id(&options.app_id)
        || !id(&options.config_id)
        || !label(&options.app_name)
        || !label(&options.label)
        || !options.state_dir.is_absolute()
    {
        return Err(err("SYNC_CONFIG"));
    }
    options.app_name = options.app_name.trim().into();
    options.label = options.label.trim().into();
    let key = hash(format!("{}:{}", options.app_id, options.config_id).as_bytes());
    let directory = options
        .state_dir
        .join(format!("ai-config-sync-{}", &key[..16]));
    platform::private_dir(&directory)?;
    let lease = platform::open_private(&directory.join("instance.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lease).map_err(|_| err("SYNC_BUSY"))?;
    let state = state::State::load(&directory.join("version.json"), adapter.as_ref())?;
    let (listener, socket_guard) = transport::bind(&directory)?;
    let shared = Arc::new(Shared {
        options,
        adapter,
        state: Mutex::new(state),
        status: Mutex::new(json!({
            "registered": false, "enabled": false, "auto_sync": false,
            "manager_auto_sync": false, "last_synced_at": null,
            "error_code": "AGENT_UNAVAILABLE", "running": false,
        })),
        registration: Mutex::new(()),
        socket: socket_guard.path.clone(),
        manager,
        control: Arc::new(transport::Control::default()),
        _lease: lease,
    });
    let owner = Arc::new(Owner {
        shared: shared.clone(),
    });
    let server = shared.clone();
    thread::Builder::new()
        .name("ai-config-callback".into())
        .spawn(move || transport::serve(listener, socket_guard, &server))
        .map_err(|_| err("SYNC_CONFIG"))?;
    thread::Builder::new()
        .name("ai-config-observer".into())
        .spawn(move || {
            while !shared.control.stopped() {
                if let Err(code) = shared.observe() {
                    shared.set_error(&code);
                } else {
                    let _ = shared.register();
                }
                shared.control.wait(observe_interval);
            }
        })
        .map_err(|_| err("SYNC_CONFIG"))?;
    Ok(SyncHandle(owner))
}

impl SyncHandle {
    /// Refresh registration and return a sanitized view; offline is an error_code.
    pub fn status(&self) -> Result<Value> {
        let shared = &self.0.shared;
        let _ = shared.register();
        shared.cached_status()
    }

    pub fn sync_now(&self) -> Result<Value> {
        self.0.shared.observe()?;
        self.0.shared.operation(json!({
            "op": "sync_run_application", "app_id": self.0.shared.options.app_id,
        }))
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<Value> {
        let options = &self.0.shared.options;
        self.0.shared.operation(json!({
            "op": "sync_set_item_enabled",
            "id": format!("app:{}:{}", options.app_id, options.config_id),
            "enabled": enabled,
        }))
    }

    pub fn set_auto_sync(&self, enabled: bool) -> Result<Value> {
        self.0.shared.operation(json!({
            "op": "sync_set_application_auto_sync",
            "app_id": self.0.shared.options.app_id, "enabled": enabled,
        }))
    }

    /// Call after releasing the application's ordinary settings mutation lock.
    pub fn local_changed(&self) -> Result<()> {
        let result = self.0.shared.observe();
        if let Err(code) = &result {
            self.0.shared.set_error(code);
        }
        result
    }
}

impl Shared {
    fn observe(&self) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| err("SYNC_CONFIG"))?
            .snapshot(self.adapter.as_ref())?;
        Ok(())
    }

    fn cached_status(&self) -> Result<Value> {
        self.status
            .lock()
            .map(|status| status.clone())
            .map_err(|_| err("SYNC_CONFIG"))
    }

    fn set_error(&self, code: &str) {
        if let Ok(mut status) = self.status.lock() {
            status["error_code"] = Value::String(fixed_error(code));
            status["running"] = Value::Bool(false);
        }
    }

    fn item<'a>(&self, view: &'a Value) -> Option<&'a Value> {
        view.get("items")?.as_array()?.iter().find(|item| {
            item["kind"] == "application"
                && item["application"]["app_id"] == self.options.app_id
                && item["application"]["config_id"] == self.options.config_id
        })
    }

    fn accept_view(&self, view: &Value) -> Result<Value> {
        if !view["items"].is_array() || !view["policy"].is_object() {
            return Err(err("AGENT_PROTOCOL"));
        }
        let item = self.item(view);
        // Records are newest first. An unrelated application's manual run must
        // not replace this application's result; a manager-wide run can apply.
        let records = view["records"].as_array();
        let applicable = |record: &&Value| {
            record["app_id"] == self.options.app_id
                || (record["app_id"].is_null()
                    && item.is_some_and(|item| {
                        item["enabled"] == true
                            && (record["trigger"] != "scheduled"
                                || item["auto_sync"].as_bool().unwrap_or(true))
                    }))
        };
        let latest = records.and_then(|records| records.iter().find(applicable));
        let legacy = records.is_none();
        let last_synced = records
            .and_then(|records| {
                records
                    .iter()
                    .filter(applicable)
                    .find(|record| record["state"] == "success")
            })
            .map(|record| &record["completed_at"])
            .or_else(|| legacy.then_some(&view["status"]["last_synced_at"]))
            .and_then(Value::as_str)
            .filter(|value| state::valid_time(value));
        let deferred = view["deferred"].as_array().and_then(|deferred| {
            let id = item?.get("id")?;
            deferred.iter().find(|entry| entry.get("id") == Some(id))
        });
        let code = deferred
            .and_then(|entry| entry["reason"].as_str())
            .map(fixed_error)
            .or_else(|| {
                latest
                    .filter(|record| record["state"] == "error")
                    .map(|record| {
                        fixed_error(record["error_code"].as_str().unwrap_or("SYNC_FAILED"))
                    })
            })
            .or_else(|| (legacy && view["status"]["state"] == "error").then(|| err("SYNC_FAILED")));
        let status = json!({
            "registered": item.is_some(),
            "enabled": item.and_then(|item| item["enabled"].as_bool()).unwrap_or(false),
            "auto_sync": item.map(|item| item["auto_sync"].as_bool().unwrap_or(true)).unwrap_or(false),
            "manager_auto_sync": view["policy"]["auto_sync_enabled"].as_bool().unwrap_or(false),
            "last_synced_at": last_synced,
            "error_code": code,
            "running": view["running"].as_bool().unwrap_or(false),
        });
        *self.status.lock().map_err(|_| err("SYNC_CONFIG"))? = status.clone();
        Ok(status)
    }

    fn request(&self, op: Value) -> Result<Sensitive> {
        transport::manager_request(
            self.manager
                .as_ref()
                .ok_or_else(|| err("AGENT_UNAVAILABLE"))?,
            &op,
            &self.control,
        )
    }

    fn register(&self) -> Result<()> {
        let _guard = self.registration.lock().map_err(|_| err("SYNC_CONFIG"))?;
        let result = self.register_locked();
        if let Err(code) = &result {
            self.set_error(code);
        }
        result
    }

    fn register_locked(&self) -> Result<()> {
        let view = self.request(json!({"op": "sync_read"}))?;
        self.accept_view(&view.0)?;
        let existing = self.item(&view.0);
        let interface = json!({"socket_path": self.socket, "path": "/config"});
        let matches = existing.is_some_and(|item| {
            item["label"] == self.options.label
                && item["application"]["app_name"] == self.options.app_name
                && item["application"]["sync_interface"] == interface
        });
        if !matches {
            let view = self.request(json!({
                "op": "sync_register_application",
                "request": {
                    "app_id": self.options.app_id, "app_name": self.options.app_name,
                    "config_id": self.options.config_id, "label": self.options.label,
                    "sync_interface": interface,
                    "enabled": existing.and_then(|item| item["enabled"].as_bool()).unwrap_or(true),
                },
            }))?;
            // auto_sync is manager-owned and retained by registration; omitting
            // it also preserves the original ApplicationRegistration wire API.
            self.accept_view(&view.0)?;
        }
        Ok(())
    }

    fn operation(&self, op: Value) -> Result<Value> {
        let result = (|| {
            self.register()?;
            let view = self.request(op)?;
            self.accept_view(&view.0)
        })();
        if let Err(code) = &result {
            self.set_error(code);
        }
        result
    }
}

pub fn digest(value: &Value) -> Result<String> {
    Ok(hash(&canonical(value)?))
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct Canonical<'a>(&'a Value);
impl Serialize for Canonical<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(map) => {
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort_unstable();
                let mut object = serializer.serialize_map(Some(keys.len()))?;
                for key in keys {
                    object.serialize_entry(key, &Canonical(&map[key]))?;
                }
                object.end()
            }
            Value::Array(values) => {
                let mut array = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    array.serialize_element(&Canonical(value))?;
                }
                array.end()
            }
            value => value.serialize(serializer),
        }
    }
}

fn canonical(value: &Value) -> Result<Zeroizing<Vec<u8>>> {
    struct Bounded(Zeroizing<Vec<u8>>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len() + bytes.len() > MAX_CONTENT {
                return Err(std::io::Error::other("limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded(Zeroizing::new(Vec::new()));
    serde_json::to_writer(&mut writer, &Canonical(value))
        .map_err(|_| err("SYNC_FILE_TOO_LARGE"))?;
    Ok(writer.0)
}

pub(crate) struct Sensitive(Value);
impl Drop for Sensitive {
    fn drop(&mut self) {
        fn wipe(value: &mut Value) {
            match value {
                Value::String(text) => text.zeroize(),
                Value::Array(values) => values.iter_mut().for_each(wipe),
                Value::Object(map) => {
                    for (mut key, mut value) in std::mem::take(map) {
                        key.zeroize();
                        wipe(&mut value);
                    }
                }
                _ => {}
            }
        }
        wipe(&mut self.0);
    }
}

fn err(code: &str) -> String {
    code.into()
}

fn adapter_error(code: &str) -> String {
    match code {
        "SYNC_LOCAL_CHANGED" | "SYNC_BUSY" | "SYNC_FILE_TOO_LARGE" => err(code),
        _ => err("SYNC_CONFIG"),
    }
}

fn fixed_error(code: &str) -> String {
    match code {
        "SYNC_LOCAL_CHANGED"
        | "SYNC_CONFIG"
        | "AGENT_UNAVAILABLE"
        | "AGENT_PROTOCOL"
        | "SYNC_BUSY"
        | "SYNC_FILE_TOO_LARGE"
        | "SYNC_CALLBACK"
        | "SYNC_APPLICATION_OFFLINE"
        | "SYNC_FAILED"
        | "SYNC_VAULT"
        | "SYNC_SETTINGS"
        | "SYNC_DISABLED"
        | "SYNC_NOT_CONFIGURED"
        | "SYNC_ITEM_MISSING"
        | "SYNC_AUTH"
        | "SYNC_NETWORK"
        | "SYNC_CONFLICT"
        | "SYNC_DECRYPT"
        | "UNAUTHORIZED"
        | "AGENT_TOKEN"
        | "SYNC_APPLICATION"
        | "SYNC_INTERFACE"
        | "SYNC_REPOSITORY"
        | "SYNC_GIT" => err(code),
        _ => err("SYNC_FAILED"),
    }
}

#[cfg(test)]
mod tests;
