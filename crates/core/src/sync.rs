use super::*;
pub use crate::sync_application::SyncInterface;
use argon2::{Algorithm, Argon2, Params, Version};
use serde_json::{json, Value as JsonValue};
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::Duration;
#[path = "sync_bundle.rs"]
mod bundle;
#[path = "sync_git.rs"]
mod git;

pub const SYNC_SCHEMA_VERSION: u32 = 1;
pub const SYNC_REMOTE_PATH: &str = ".ai-config-manager/sync.enc";
pub const AI_CONFIG_ITEM_ID: &str = "ai-config";
const MAX_ITEM_BYTES: u64 = 1024 * 1024;
const MAX_BUNDLE_BYTES: u64 = 8 * 1024 * 1024;
const SYNC_AAD: &[u8] = b"ai-config-manager:sync:v1";
const SECRET_SERVICE: &str = "ai-config-manager-sync";
const GITHUB_TOKEN_ACCOUNT: &str = "github-token-v1";
const SYNC_PASSWORD_ACCOUNT: &str = "sync-encryption-password-v1";

pub trait SyncSecrets: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, value: &str) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncProvider {
    #[default]
    Github,
    Git,
}

pub fn default_git_branch() -> String {
    "main".into()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryTestRequest {
    #[serde(default)]
    pub provider: SyncProvider,
    pub repo: String,
    #[serde(default = "default_git_branch")]
    pub git_branch: String,
    pub github_token: Option<String>,
}
impl Drop for RepositoryTestRequest {
    fn drop(&mut self) {
        self.github_token.zeroize();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryTestResult {
    pub ok: bool,
    pub readable: bool,
    pub writable: bool,
    pub cleanup_ok: bool,
    pub code: String,
    pub latency_ms: u64,
    pub probe_ref: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRepositoryRequest {
    /// `owner/name`, a bare `name`, or a github.com URL. A bare name creates the
    /// repository under the authenticated account.
    pub repo: String,
    pub github_token: Option<String>,
}
impl Drop for CreateRepositoryRequest {
    fn drop(&mut self) {
        self.github_token.zeroize();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateRepositoryResult {
    pub repo: String,
    pub url: String,
    pub private: bool,
}

pub struct NativeSyncSecrets;
impl Default for NativeSyncSecrets {
    fn default() -> Self {
        Self
    }
}
impl NativeSyncSecrets {
    pub fn new() -> Self {
        Self
    }
    fn entry(account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SECRET_SERVICE, account).map_err(|_| err("SYNC_VAULT"))
    }
}
impl SyncSecrets for NativeSyncSecrets {
    fn get(&self, account: &str) -> Result<Option<String>> {
        match Self::entry(account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(err("SYNC_VAULT")),
        }
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        Self::entry(account)?
            .set_password(value)
            .map_err(|_| err("SYNC_VAULT"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncKind {
    AiConfig,
    File,
    Application,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SyncItem {
    pub id: String,
    pub label: String,
    pub kind: SyncKind,
    pub path: Option<String>,
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_sync: bool,
    #[serde(default)]
    pub application: Option<SyncApplication>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SyncApplication {
    pub app_id: String,
    pub app_name: String,
    pub config_id: String,
    pub sync_interface: SyncInterface,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationRegistration {
    pub app_id: String,
    pub app_name: String,
    pub config_id: String,
    pub label: String,
    pub sync_interface: SyncInterface,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeferredItem {
    pub id: String,
    pub label: String,
    pub kind: SyncKind,
    /// Fixed protocol code describing why the item waits for the next attempt.
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SyncPolicy {
    pub auto_sync_enabled: bool,
    pub sync_interval_minutes: u32,
    pub history_retention_days: u32,
}
impl Default for SyncPolicy {
    fn default() -> Self {
        Self {
            auto_sync_enabled: true,
            sync_interval_minutes: 10,
            history_retention_days: 30,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncTrigger {
    Manual,
    Agent,
    Scheduled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SyncRecord {
    pub id: String,
    #[serde(default)]
    pub app_id: Option<String>,
    pub started_at: String,
    pub completed_at: String,
    pub trigger: SyncTrigger,
    pub state: SyncState,
    pub repo: String,
    pub item_count: usize,
    pub uploaded_count: usize,
    pub downloaded_count: usize,
    pub conflict_count: usize,
    pub error_code: Option<String>,
    pub remote_sha: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    Never,
    Running,
    Success,
    Error,
}
impl Default for SyncState {
    fn default() -> Self {
        Self::Never
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SyncStatus {
    pub state: SyncState,
    pub last_synced_at: Option<String>,
    pub message: String,
    pub item_count: usize,
    pub remote_path: String,
    pub remote_sha: Option<String>,
    pub remote_url: Option<String>,
}
impl Default for SyncStatus {
    fn default() -> Self {
        Self {
            state: SyncState::Never,
            last_synced_at: None,
            message: String::new(),
            item_count: 0,
            remote_path: SYNC_REMOTE_PATH.into(),
            remote_sha: None,
            remote_url: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncView {
    pub provider: SyncProvider,
    pub git_branch: String,
    pub repo: String,
    pub token_present: bool,
    pub encryption_secret_present: bool,
    pub items: Vec<SyncItem>,
    pub status: SyncStatus,
    pub running: bool,
    pub warnings: Vec<String>,
    pub policy: SyncPolicy,
    pub next_sync_at: Option<String>,
    pub records: Vec<SyncRecord>,
    /// Enabled items that could not be read or updated on this attempt. Their
    /// remote copy is retained and the next attempt retries them.
    pub deferred: Vec<DeferredItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncConfig {
    schema_version: u32,
    #[serde(default)]
    provider: SyncProvider,
    #[serde(default = "default_git_branch")]
    git_branch: String,
    #[serde(default)]
    repo: String,
    #[serde(default = "default_items")]
    items: Vec<SyncItem>,
    #[serde(default)]
    status: SyncStatus,
    #[serde(default)]
    policy: SyncPolicy,
    #[serde(default)]
    next_sync_at: Option<String>,
    #[serde(default)]
    records: Vec<SyncRecord>,
    #[serde(default)]
    active_record: Option<SyncRecord>,
    #[serde(default)]
    deferred: Vec<DeferredItem>,
    /// Consecutive deferred/transient attempts; drives the retry backoff.
    #[serde(default)]
    retry_attempts: u32,
    /// Set when a local AI configuration save should be uploaded soon. It is
    /// consumed by the next scheduled attempt, including one already running.
    #[serde(default)]
    pending_sync: bool,
}

fn default_items() -> Vec<SyncItem> {
    vec![default_ai_item()]
}
fn default_ai_item() -> SyncItem {
    SyncItem {
        id: AI_CONFIG_ITEM_ID.into(),
        label: "AI configuration".into(),
        kind: SyncKind::AiConfig,
        path: None,
        enabled: true,
        auto_sync: true,
        application: None,
    }
}

pub struct SyncStore {
    path: PathBuf,
    store: Arc<Store>,
    secrets: Arc<dyn SyncSecrets>,
    state: Mutex<SyncConfig>,
    running: AtomicBool,
    operation: Mutex<()>,
    warnings: Mutex<Vec<String>>,
    blocked: Mutex<Option<String>>,
    api_base: String,
}

impl SyncStore {
    #[cfg(test)]
    fn with_api_base(mut self, api_base: String) -> Self {
        self.api_base = api_base;
        self
    }
    pub fn new(path: PathBuf, store: Arc<Store>, secrets: Arc<dyn SyncSecrets>) -> Self {
        let (mut config, warnings, blocked) = load_config(&path);
        let original_records = config.records.clone();
        let interrupted = config.active_record.is_some();
        ensure_ai_item(&mut config);
        if config.status.state == SyncState::Running {
            config.status.state = SyncState::Error;
            config.status.message = "The previous sync was interrupted.".into();
            if let Some(mut record) = config.active_record.take() {
                record.completed_at = chrono::Utc::now().to_rfc3339();
                record.state = SyncState::Error;
                record.error_code = Some("SYNC_INTERRUPTED".into());
                config.records.insert(0, record);
            }
        }
        let now = chrono::Utc::now();
        if interrupted {
            // Retry an interrupted attempt soon instead of waiting a full interval.
            config.next_sync_at = retry_deadline(&config.policy, config.retry_attempts, now);
        }
        prune_records(&mut config, now);
        let persist_cleanup =
            blocked.is_none() && (interrupted || original_records != config.records);
        let result = Self {
            path,
            store,
            secrets,
            state: Mutex::new(config),
            running: AtomicBool::new(false),
            operation: Mutex::new(()),
            warnings: Mutex::new(warnings),
            blocked: Mutex::new(blocked),
            api_base: "https://api.github.com".into(),
        };
        if persist_cleanup {
            if let Ok(state) = result.state.lock() {
                if result.save_locked(&state).is_err() {
                    result.warn_status_save();
                }
            }
        }
        result
    }
    pub fn view(&self) -> SyncView {
        let config = self
            .state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| SyncConfig {
                schema_version: SYNC_SCHEMA_VERSION,
                provider: SyncProvider::default(),
                git_branch: default_git_branch(),
                repo: String::new(),
                items: default_items(),
                status: SyncStatus::default(),
                policy: SyncPolicy::default(),
                next_sync_at: None,
                records: vec![],
                active_record: None,
                deferred: vec![],
                retry_attempts: 0,
                pending_sync: false,
            });
        let mut warnings = self
            .warnings
            .lock()
            .map(|warnings| warnings.clone())
            .unwrap_or_default();
        let (token_present, encryption_secret_present) = if config.repo.is_empty() {
            (false, false)
        } else {
            let token_present = match if config.provider == SyncProvider::Github {
                self.secrets.get(GITHUB_TOKEN_ACCOUNT)
            } else {
                Ok(None)
            } {
                Ok(Some(mut value)) => {
                    value.zeroize();
                    true
                }
                Ok(None) => false,
                Err(error) => {
                    push_unique(&mut warnings, &format!("{}: {}", error.code, error.message));
                    false
                }
            };
            let encryption_secret_present = match self.secrets.get(SYNC_PASSWORD_ACCOUNT) {
                Ok(Some(mut value)) => {
                    value.zeroize();
                    true
                }
                Ok(None) => false,
                Err(error) => {
                    push_unique(&mut warnings, &format!("{}: {}", error.code, error.message));
                    false
                }
            };
            (token_present, encryption_secret_present)
        };
        SyncView {
            provider: config.provider,
            git_branch: config.git_branch,
            repo: config.repo,
            token_present,
            encryption_secret_present,
            items: config.items,
            status: config.status,
            running: self.running.load(Ordering::SeqCst),
            warnings,
            policy: config.policy,
            next_sync_at: config.next_sync_at,
            records: config.records,
            deferred: config.deferred,
        }
    }
    pub fn save_settings(
        &self,
        repo: &str,
        github_token: Option<String>,
        encryption_password: Option<String>,
    ) -> Result<SyncView> {
        self.save_repository_settings(
            SyncProvider::Github,
            repo,
            "main",
            github_token,
            encryption_password,
        )
    }
    pub fn save_repository_settings(
        &self,
        provider: SyncProvider,
        repo: &str,
        git_branch: &str,
        github_token: Option<String>,
        encryption_password: Option<String>,
    ) -> Result<SyncView> {
        let token = github_token.map(Zeroizing::new);
        let password = encryption_password.map(Zeroizing::new);
        let _operation = self.lock_operation()?;
        self.check_blocked()?;
        let repo = normalize_target(provider, repo)?;
        let git_branch = git::validate_branch(git_branch)?;
        if provider == SyncProvider::Git && token.is_some() {
            return Err(err("SYNC_TOKEN"));
        }
        if let Some(token) = token.as_ref() {
            let trimmed = token.trim();
            if trimmed.is_empty() || trimmed.len() > 8192 || trimmed.chars().any(char::is_control) {
                return Err(err("SYNC_TOKEN"));
            }
        }
        if let Some(password) = password.as_ref() {
            if password.chars().count() < 8 || password.len() > 1024 {
                return Err(err("SYNC_PASSWORD"));
            }
        }
        if let Some(token) = token.as_ref() {
            self.secrets.set(GITHUB_TOKEN_ACCOUNT, token.trim())?;
        }
        if let Some(password) = password.as_ref() {
            self.secrets.set(SYNC_PASSWORD_ACCOUNT, password)?;
        }
        let mut state = self.lock_state()?;
        let mut next = state.clone();
        if next.repo != repo || next.provider != provider || next.git_branch != git_branch {
            next.status = SyncStatus::default();
            next.deferred.clear();
            next.retry_attempts = 0;
            next.next_sync_at = next_deadline(&next.policy, chrono::Utc::now());
        }
        next.provider = provider;
        next.git_branch = git_branch;
        next.repo = repo;
        self.save_locked(&next)?;
        *state = next;
        drop(state);
        Ok(self.view())
    }
    pub fn test_repository(
        &self,
        mut request: RepositoryTestRequest,
    ) -> Result<RepositoryTestResult> {
        let _operation = self.lock_operation()?;
        let started = std::time::Instant::now();
        let repo = normalize_target(request.provider, &request.repo)?;
        let branch = git::validate_branch(&request.git_branch)?;
        let (url, token) = match request.provider {
            SyncProvider::Github => {
                let token = match request.github_token.take() {
                    Some(token) => Some(token),
                    None => self.secrets.get(GITHUB_TOKEN_ACCOUNT)?,
                };
                let token = token
                    .map(Zeroizing::new)
                    .ok_or_else(|| err("SYNC_NOT_CONFIGURED"))?;
                if token.trim().is_empty()
                    || token.len() > 8192
                    || token.chars().any(char::is_control)
                {
                    return Err(err("SYNC_TOKEN"));
                }
                (format!("https://github.com/{repo}.git"), Some(token))
            }
            SyncProvider::Git => {
                if request.github_token.is_some() {
                    return Err(err("SYNC_TOKEN"));
                }
                (repo, None)
            }
        };
        let probe =
            match git::GitRepository::new(&url, &branch, token.as_deref().map(String::as_str)) {
                Ok(repository) => repository.probe(),
                Err(error) => git::GitProbe {
                    readable: false,
                    writable: false,
                    cleanup_ok: true,
                    code: error.code,
                    probe_ref: None,
                },
            };
        Ok(RepositoryTestResult {
            ok: probe.readable && probe.writable && probe.cleanup_ok && probe.code == "OK",
            readable: probe.readable,
            writable: probe.writable,
            cleanup_ok: probe.cleanup_ok,
            code: probe.code,
            probe_ref: probe.probe_ref,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }
    pub fn create_github_repository(
        &self,
        mut request: CreateRepositoryRequest,
    ) -> Result<CreateRepositoryResult> {
        let _operation = self.lock_operation()?;
        self.check_blocked()?;
        let (owner, name) = parse_new_repo(&request.repo)?;
        let token = match request.github_token.take() {
            Some(token) => token,
            None => self
                .secrets
                .get(GITHUB_TOKEN_ACCOUNT)?
                .ok_or_else(|| err("SYNC_NOT_CONFIGURED"))?,
        };
        let token = Zeroizing::new(token);
        let token = token.trim();
        if token.is_empty() || token.len() > 8192 || token.chars().any(char::is_control) {
            return Err(err("SYNC_TOKEN"));
        }
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("ai-config-manager/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| err("GITHUB_NETWORK"))?;
        // Resolve whether the requested owner is the authenticated account or an
        // organization before creating anything; POSTing to the wrong endpoint
        // would either fail or create an unrelated repository.
        let authenticated =
            bundle::github_request(client.get(format!("{}/user", self.api_base)), token)
                .send()
                .map_err(map_github_error)?;
        if !authenticated.status().is_success() {
            return Err(github_status_error(authenticated.status().as_u16()));
        }
        let login = bundle::read_json(authenticated)?
            .get("login")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| err("GITHUB_REJECTED"))?
            .to_string();
        let url = match owner.as_deref() {
            Some(owner) if !owner.eq_ignore_ascii_case(&login) => {
                format!("{}/orgs/{owner}/repos", self.api_base)
            }
            _ => format!("{}/user/repos", self.api_base),
        };
        let response = bundle::github_request(client.post(url), token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(json!({ "name": name, "private": true }).to_string())
            .send()
            .map_err(map_github_error)?;
        if response.status().as_u16() != 201 {
            return Err(github_status_error(response.status().as_u16()));
        }
        let metadata = bundle::read_json(response)?;
        let repo = metadata
            .get("full_name")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| err("GITHUB_REJECTED"))
            .and_then(|value| normalize_repo(value).map_err(|_| err("GITHUB_REJECTED")))?;
        if owner.as_deref().is_some_and(|owner| {
            !repo
                .split_once('/')
                .is_some_and(|(name, _)| name.eq_ignore_ascii_case(owner))
        }) {
            return Err(err("GITHUB_REJECTED"));
        }
        Ok(CreateRepositoryResult {
            repo,
            url: metadata
                .get("html_url")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string(),
            private: metadata
                .get("private")
                .and_then(JsonValue::as_bool)
                .unwrap_or(true),
        })
    }
    pub fn add_item(&self, label: Option<String>, path: &str, enabled: bool) -> Result<SyncView> {
        let _operation = self.lock_operation()?;
        let path = path.trim();
        let path_buf = validate_file_path(path)?;
        let label = label.unwrap_or_default();
        let label = label.trim();
        let label = if label.is_empty() {
            path_buf
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Configuration")
                .to_string()
        } else {
            if label.len() > 200 || label.chars().any(char::is_control) {
                return Err(err("SYNC_PATH"));
            }
            label.to_string()
        };
        let mut state = self.lock_state()?;
        self.check_blocked()?;
        if state
            .items
            .iter()
            .any(|item| item.path.as_deref() == Some(path))
        {
            return Err(err("SYNC_DUPLICATE"));
        }
        let mut id_bytes = [0u8; 16];
        OsRng.fill_bytes(&mut id_bytes);
        let id = id_bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut next = state.clone();
        next.items.push(SyncItem {
            id,
            label,
            kind: SyncKind::File,
            path: Some(path.into()),
            enabled,
            auto_sync: true,
            application: None,
        });
        self.save_locked(&next)?;
        *state = next;
        drop(state);
        Ok(self.view())
    }
    pub fn remove_item(&self, id: &str) -> Result<SyncView> {
        let _operation = self.lock_operation()?;
        if id == AI_CONFIG_ITEM_ID {
            return Err(err("SYNC_REQUIRED"));
        }
        let mut state = self.lock_state()?;
        self.check_blocked()?;
        let before = state.items.len();
        let mut next = state.clone();
        next.items.retain(|item| item.id != id);
        next.deferred.retain(|item| item.id != id);
        if next.items.len() == before {
            return Err(err("SYNC_ITEM_MISSING"));
        }
        self.save_locked(&next)?;
        *state = next;
        drop(state);
        Ok(self.view())
    }
    pub fn set_item_enabled(&self, id: &str, enabled: bool) -> Result<SyncView> {
        let _operation = self.lock_operation()?;
        let mut state = self.lock_state()?;
        self.check_blocked()?;
        let mut next = state.clone();
        let item = next
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| err("SYNC_ITEM_MISSING"))?;
        item.enabled = enabled;
        if !enabled {
            next.deferred.retain(|deferred| deferred.id != id);
        }
        self.save_locked(&next)?;
        *state = next;
        drop(state);
        Ok(self.view())
    }
    pub fn register_application(&self, request: ApplicationRegistration) -> Result<SyncView> {
        let _operation = self.lock_operation()?;
        self.check_blocked()?;
        if !valid_id(&request.app_id)
            || !valid_id(&request.config_id)
            || !valid_label(&request.app_name)
            || !valid_label(&request.label)
        {
            return Err(err("SYNC_APPLICATION"));
        }
        crate::sync_application::validate_interface(&request.sync_interface)?;
        let item_id = format!("app:{}:{}", request.app_id, request.config_id);
        let mut item = SyncItem {
            id: item_id.clone(),
            label: request.label.trim().into(),
            kind: SyncKind::Application,
            path: None,
            enabled: request.enabled,
            auto_sync: true,
            application: Some(SyncApplication {
                app_id: request.app_id,
                app_name: request.app_name.trim().into(),
                config_id: request.config_id,
                sync_interface: request.sync_interface,
            }),
        };
        let mut state = self.lock_state()?;
        let mut next = state.clone();
        if let Some(existing) = next
            .items
            .iter_mut()
            .find(|existing| existing.id == item.id)
        {
            // Startup upserts update the local callback, never the user's choices.
            item.enabled = existing.enabled;
            item.auto_sync = existing.auto_sync;
            if *existing == item {
                drop(state);
                return Ok(self.view());
            }
            *existing = item;
        } else {
            next.items.push(item);
        }
        next.deferred.retain(|deferred| deferred.id != item_id);
        self.save_locked(&next)?;
        *state = next;
        drop(state);
        Ok(self.view())
    }
    pub fn set_application_auto_sync(&self, app_id: &str, enabled: bool) -> Result<SyncView> {
        if !valid_id(app_id) {
            return Err(err("SYNC_APPLICATION"));
        }
        let _operation = self.lock_operation()?;
        self.check_blocked()?;
        let mut state = self.lock_state()?;
        let mut next = state.clone();
        let mut found = false;
        for item in &mut next.items {
            if belongs_to_application(item, app_id) {
                item.auto_sync = enabled;
                found = true;
            }
        }
        if !found {
            return Err(err("SYNC_ITEM_MISSING"));
        }
        if !enabled {
            next.deferred.retain(|entry| {
                !next
                    .items
                    .iter()
                    .any(|item| item.id == entry.id && belongs_to_application(item, app_id))
            });
        }
        self.save_locked(&next)?;
        *state = next;
        drop(state);
        Ok(self.view())
    }
    pub fn poll_ai_change(&self) -> Result<bool> {
        if !self.store.has_dirty_flag() {
            return Ok(false);
        }
        if self.request_sync_after_config_change()? {
            self.store.clear_dirty_flag();
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn request_sync_after_config_change(&self) -> Result<bool> {
        let mut state = self.lock_state()?;
        if !state.policy.auto_sync_enabled
            || state.repo.is_empty()
            || !state
                .items
                .iter()
                .any(|item| item.kind == SyncKind::AiConfig && item.enabled && item.auto_sync)
        {
            return Ok(false);
        }
        let now = chrono::Utc::now();
        let desired = now + chrono::Duration::seconds(2);
        let mut next = state.clone();
        next.pending_sync = true;
        let earlier = state.next_sync_at.as_deref().is_none_or(|value| {
            chrono::DateTime::parse_from_rfc3339(value).map_or(true, |deadline| deadline > desired)
        });
        if earlier {
            next.next_sync_at = Some(desired.to_rfc3339());
        }
        self.save_locked(&next)?;
        *state = next;
        Ok(true)
    }
    pub fn save_policy(&self, policy: SyncPolicy) -> Result<SyncView> {
        validate_policy(&policy)?;
        let _operation = self.lock_operation()?;
        self.check_blocked()?;
        let mut state = self.lock_state()?;
        let mut next = state.clone();
        let now = chrono::Utc::now();
        next.next_sync_at = if next.deferred.is_empty() {
            next_deadline(&policy, now)
        } else {
            retry_deadline(&policy, next.retry_attempts, now)
        };
        next.policy = policy;
        prune_records(&mut next, chrono::Utc::now());
        self.save_locked(&next)?;
        *state = next;
        drop(state);
        Ok(self.view())
    }
    pub fn history(&self) -> Result<Vec<SyncRecord>> {
        if let Err(error) = self.cleanup() {
            if error.code != "SYNC_BUSY" {
                return Err(error);
            }
        }
        Ok(self.lock_state()?.records.clone())
    }
    pub fn cleanup(&self) -> Result<SyncView> {
        self.cleanup_at(chrono::Utc::now())
    }
    fn cleanup_at(&self, now: chrono::DateTime<chrono::Utc>) -> Result<SyncView> {
        let _operation = self.lock_operation()?;
        self.check_blocked()?;
        let mut state = self.lock_state()?;
        let mut next = state.clone();
        prune_records(&mut next, now);
        if next.records != state.records {
            self.save_locked(&next)?;
            *state = next;
        }
        let retention = state.policy.history_retention_days;
        drop(state);
        if retention > 0 {
            self.store
                .prune_history_before(now - chrono::Duration::days(retention.into()))?;
        }
        Ok(self.view())
    }
    pub fn run_due_sync(&self, now: chrono::DateTime<chrono::Utc>) -> Result<Option<SyncView>> {
        let _operation = self.lock_operation()?;
        self.check_blocked()?;
        let mut state = self.lock_state()?;
        if !state.policy.auto_sync_enabled {
            return Ok(None);
        }
        let due = match state.next_sync_at.as_deref() {
            Some(deadline) => {
                chrono::DateTime::parse_from_rfc3339(deadline).map_err(|_| err("SYNC_CONFIG"))?
                    <= now
            }
            None => {
                let mut next = state.clone();
                next.next_sync_at = next_deadline(&next.policy, now);
                self.save_locked(&next)?;
                *state = next;
                false
            }
        };
        if due
            && (state.repo.is_empty()
                || !state
                    .items
                    .iter()
                    .any(|item| item.enabled && item.auto_sync))
        {
            // Automatic sync is on by default. Before a target is saved, keep
            // rescheduling instead of recording a failure on every interval.
            let mut next = state.clone();
            next.next_sync_at = next_deadline(&next.policy, now);
            next.pending_sync = false;
            self.save_locked(&next)?;
            *state = next;
            return Ok(None);
        }
        drop(state);
        if due {
            self.run_locked(SyncTrigger::Scheduled, now, None).map(Some)
        } else {
            Ok(None)
        }
    }
    pub fn run_scheduled(&self) -> Result<Option<SyncView>> {
        self.run_due_sync(chrono::Utc::now())
    }
    pub fn sync_due(&self) -> bool {
        self.is_sync_due(chrono::Utc::now())
    }
    fn is_sync_due(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.policy.auto_sync_enabled
                && state.next_sync_at.as_deref().is_none_or(|value| {
                    chrono::DateTime::parse_from_rfc3339(value)
                        .is_ok_and(|deadline| deadline <= now)
                })
        })
    }
    pub fn run_sync(&self) -> Result<SyncView> {
        self.run_sync_with_trigger(SyncTrigger::Manual)
    }
    pub fn run_sync_with_trigger(&self, trigger: SyncTrigger) -> Result<SyncView> {
        let _operation = self.lock_operation()?;
        self.run_locked(trigger, chrono::Utc::now(), None)
    }
    pub fn run_application(&self, app_id: &str, trigger: SyncTrigger) -> Result<SyncView> {
        if !valid_id(app_id) {
            return Err(err("SYNC_APPLICATION"));
        }
        let _operation = self.lock_operation()?;
        self.check_blocked()?;
        let state = self.lock_state()?;
        if !state
            .items
            .iter()
            .any(|item| belongs_to_application(item, app_id))
        {
            return Err(err("SYNC_ITEM_MISSING"));
        }
        if !state
            .items
            .iter()
            .any(|item| item.enabled && belongs_to_application(item, app_id))
        {
            return Err(err("SYNC_DISABLED"));
        }
        drop(state);
        self.run_locked(trigger, chrono::Utc::now(), Some(app_id))
    }
    fn run_locked(
        &self,
        trigger: SyncTrigger,
        now: chrono::DateTime<chrono::Utc>,
        app_id: Option<&str>,
    ) -> Result<SyncView> {
        self.check_blocked()?;
        let items: Vec<_> = self
            .lock_state()?
            .items
            .iter()
            .filter(|item| {
                item.enabled
                    && (trigger != SyncTrigger::Scheduled || item.auto_sync)
                    && app_id.is_none_or(|app_id| belongs_to_application(item, app_id))
            })
            .cloned()
            .collect();
        self.running.store(true, Ordering::SeqCst);
        let prepare = (|| {
            let mut state = self.lock_state()?;
            let mut next = state.clone();
            next.active_record = Some(SyncRecord {
                id: random_id(),
                app_id: app_id.map(str::to_owned),
                started_at: now.to_rfc3339(),
                completed_at: now.to_rfc3339(),
                trigger,
                state: SyncState::Error,
                repo: next.repo.clone(),
                item_count: 0,
                uploaded_count: 0,
                downloaded_count: 0,
                conflict_count: 0,
                error_code: Some("SYNC_INTERRUPTED".into()),
                remote_sha: None,
            });
            if app_id.is_none() {
                next.next_sync_at = next_deadline(&next.policy, now);
                next.pending_sync = false;
            }
            next.status.state = SyncState::Running;
            self.save_locked(&next)?;
            *state = next;
            Ok(())
        })();
        if let Err(error) = prepare {
            self.running.store(false, Ordering::SeqCst);
            return Err(error);
        }
        let result = self.run_sync_inner(&items);
        let completed = chrono::Utc::now().max(now);
        // An application excluded from automatic retries still needs a truthful
        // result for its own manual run; only retryable entries go to `deferred`.
        // Attribution is exact only for application-scoped runs.
        let manual_failure = if app_id.is_some() {
            result.as_ref().ok().and_then(|deferred| {
                deferred.iter().find(|entry| {
                    items
                        .iter()
                        .any(|item| item.id == entry.id && !item.auto_sync)
                })
            })
        } else {
            None
        };
        if let Ok(mut state) = self.state.lock() {
            if let Some(mut record) = state.active_record.take() {
                record.completed_at = completed.to_rfc3339();
                record.state = if result.is_ok() && manual_failure.is_none() {
                    SyncState::Success
                } else {
                    SyncState::Error
                };
                record.error_code = manual_failure
                    .map(|failure| failure.reason.clone())
                    .or_else(|| result.as_ref().err().map(|error| error.code.clone()));
                if result.is_ok() && manual_failure.is_none() {
                    record.remote_sha = state.status.remote_sha.clone();
                }
                state.records.insert(0, record);
            }
            match &result {
                Ok(deferred) => {
                    state
                        .deferred
                        .retain(|entry| !items.iter().any(|item| item.id == entry.id));
                    state.deferred.extend(
                        deferred
                            .iter()
                            .filter(|entry| {
                                items
                                    .iter()
                                    .any(|item| item.id == entry.id && item.auto_sync)
                            })
                            .cloned(),
                    );
                }
                Err(error) => {
                    if error.code == "SYNC_NOT_CONFIGURED" {
                        state
                            .deferred
                            .retain(|entry| !items.iter().any(|item| item.id == entry.id));
                    }
                }
            }
            let retrying = match &result {
                Ok(_) => !state.deferred.is_empty(),
                Err(error) => transient_failure(&error.code),
            };
            let policy = state.policy.clone();
            // An application's manual action must not postpone other applications.
            if app_id.is_none() {
                state.retry_attempts = if retrying {
                    state.retry_attempts.saturating_add(1)
                } else {
                    0
                };
                state.next_sync_at = if state.pending_sync {
                    // A save landed during this attempt. Run once more soon so
                    // the new configuration is uploaded, then resume the interval.
                    state.pending_sync = false;
                    Some((completed + chrono::Duration::seconds(2)).to_rfc3339())
                } else if retrying {
                    retry_deadline(&policy, state.retry_attempts, completed)
                } else {
                    next_deadline(&policy, completed)
                };
            }
            prune_records(&mut state, chrono::Utc::now());
            if self.save_locked(&state).is_err() {
                self.warn_status_save();
            }
        }
        self.running.store(false, Ordering::SeqCst);
        result.map(|_| self.view())
    }
    fn run_sync_inner(&self, items: &[SyncItem]) -> Result<Vec<DeferredItem>> {
        self.check_blocked()?;
        let (provider, repo, git_branch) = {
            let state = self.lock_state()?;
            (state.provider, state.repo.clone(), state.git_branch.clone())
        };
        if repo.is_empty() {
            return self.fail_sync(err("SYNC_NOT_CONFIGURED"));
        }
        if items.is_empty() {
            return self.fail_sync(err("SYNC_NOT_CONFIGURED"));
        }
        let token = match if provider == SyncProvider::Github {
            self.secrets.get(GITHUB_TOKEN_ACCOUNT)
        } else {
            Ok(Some(String::new()))
        } {
            Ok(Some(token)) => Zeroizing::new(token),
            Ok(None) => return self.fail_sync(err("SYNC_NOT_CONFIGURED")),
            Err(error) => return self.fail_sync(error),
        };
        let password = match self.secrets.get(SYNC_PASSWORD_ACCOUNT) {
            Ok(Some(password)) => Zeroizing::new(password),
            Ok(None) => return self.fail_sync(err("SYNC_NOT_CONFIGURED")),
            Err(error) => return self.fail_sync(error),
        };
        self.set_status(SyncStatus {
            state: SyncState::Running,
            last_synced_at: self.last_synced_at(),
            message: "Sync in progress.".into(),
            item_count: items.len(),
            remote_path: SYNC_REMOTE_PATH.into(),
            remote_sha: None,
            remote_url: None,
        });
        let outcome = match self.synchronize_target(
            provider,
            &repo,
            &git_branch,
            token.as_str(),
            password.as_str(),
            items,
        ) {
            Ok(outcome) => outcome,
            Err(error) => return self.fail_sync(error),
        };
        if let Some(record) = self.lock_state()?.active_record.as_mut() {
            record.item_count = outcome.item_count;
            record.uploaded_count = outcome.uploaded_count;
            record.downloaded_count = outcome.downloaded_count;
            record.conflict_count = outcome.conflict_count;
        }
        self.set_status(SyncStatus {
            state: SyncState::Success,
            last_synced_at: Some(chrono::Utc::now().to_rfc3339()),
            message: if outcome.deferred.is_empty() {
                format!("Synced {} item(s).", outcome.item_count)
            } else {
                format!(
                    "Synced {} item(s); {} waiting for automatic retry.",
                    outcome.item_count,
                    outcome.deferred.len()
                )
            },
            item_count: outcome.item_count,
            remote_path: SYNC_REMOTE_PATH.into(),
            remote_sha: outcome.remote.sha,
            remote_url: outcome.remote.url,
        });
        Ok(outcome.deferred)
    }
    fn fail_sync<T>(&self, error: Error) -> Result<T> {
        self.set_status(SyncStatus {
            state: SyncState::Error,
            last_synced_at: self.last_synced_at(),
            message: format!("{}: {}", error.code, error.message),
            item_count: 0,
            remote_path: SYNC_REMOTE_PATH.into(),
            remote_sha: None,
            remote_url: None,
        });
        Err(error)
    }
    fn last_synced_at(&self) -> Option<String> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.status.last_synced_at.clone())
    }
    fn set_status(&self, status: SyncStatus) {
        let mut warning = None;
        if let Ok(mut state) = self.state.lock() {
            state.status = status;
            if let Err(error) = self.save_locked(&state) {
                warning = Some(format!("SYNC_STATUS_SAVE: {}", error.message));
            }
        }
        if let Some(warning) = warning {
            if let Ok(mut warnings) = self.warnings.lock() {
                push_unique(&mut warnings, &warning);
            }
        }
    }
    fn warn_status_save(&self) {
        if let Ok(mut warnings) = self.warnings.lock() {
            push_unique(
                &mut warnings,
                "SYNC_STATUS_SAVE: Sync records could not be persisted.",
            );
        }
    }
    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, SyncConfig>> {
        self.state.lock().map_err(|_| err("SYNC_STATE"))
    }
    fn lock_operation(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.operation.try_lock().map_err(|_| err("SYNC_BUSY"))
    }
    fn check_blocked(&self) -> Result<()> {
        if let Ok(blocked) = self.blocked.lock() {
            if let Some(reason) = blocked.as_ref() {
                return Err(Error {
                    code: "SYNC_CONFIG".into(),
                    message: reason.clone(),
                });
            }
        }
        Ok(())
    }
    fn save_locked(&self, state: &SyncConfig) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|_| err("IO"))?;
        }
        let bytes = serde_json::to_vec_pretty(state).map_err(|_| err("SYNC_CONFIG"))?;
        write_atomic(&self.path, &bytes)?;
        if let Ok(mut blocked) = self.blocked.lock() {
            *blocked = None;
        }
        Ok(())
    }
}

#[derive(Default)]
struct RemoteFile {
    sha: Option<String>,
    url: Option<String>,
}

fn load_config(path: &Path) -> (SyncConfig, Vec<String>, Option<String>) {
    let default = SyncConfig {
        schema_version: SYNC_SCHEMA_VERSION,
        provider: SyncProvider::default(),
        git_branch: default_git_branch(),
        repo: String::new(),
        items: default_items(),
        status: SyncStatus::default(),
        policy: SyncPolicy::default(),
        next_sync_at: None,
        records: vec![],
        active_record: None,
        deferred: vec![],
        retry_attempts: 0,
        pending_sync: false,
    };
    match read_bounded(path) {
        Ok(None) => (default, vec![], None),
        Ok(Some(bytes)) => match serde_json::from_slice::<SyncConfig>(&bytes) {
            Ok(config) if config.schema_version == SYNC_SCHEMA_VERSION && validate_policy(&config.policy).is_ok()
                && git::validate_branch(&config.git_branch).is_ok()
                && (config.repo.is_empty() || normalize_target(config.provider, &config.repo).is_ok()) => (config, vec![], None),
            _ => (
                default,
                vec!["SYNC_CONFIG: Sync settings are invalid or from an unsupported version; defaults are in use and saving is blocked until the file is repaired or removed.".into()],
                Some("The sync settings file is invalid or uses an unsupported schema version".into()),
            ),
        },
        Err(error) => (
            default,
            vec![format!(
                "SYNC_CONFIG: Sync settings could not be read ({}) ; saving is blocked until the file is repaired or removed.",
                error.code
            )],
            Some("The sync settings file could not be read".into()),
        ),
    }
}

fn ensure_ai_item(config: &mut SyncConfig) {
    if !config
        .items
        .iter()
        .any(|item| item.id == AI_CONFIG_ITEM_ID || item.kind == SyncKind::AiConfig)
    {
        config.items.insert(0, default_ai_item());
    }
}

fn push_unique(list: &mut Vec<String>, value: &str) {
    if !list.iter().any(|item| item == value) {
        list.push(value.to_string());
    }
}

fn random_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}
fn belongs_to_application(item: &SyncItem, app_id: &str) -> bool {
    item.kind == SyncKind::Application
        && item
            .application
            .as_ref()
            .is_some_and(|application| application.app_id == app_id)
}
fn valid_label(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}
fn validate_policy(policy: &SyncPolicy) -> Result<()> {
    if !(1..=1440).contains(&policy.sync_interval_minutes) || policy.history_retention_days > 3650 {
        return Err(err("SYNC_POLICY"));
    }
    Ok(())
}
fn next_deadline(policy: &SyncPolicy, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    policy.auto_sync_enabled.then(|| {
        (now + chrono::Duration::minutes(policy.sync_interval_minutes.into())).to_rfc3339()
    })
}
/// Backoff used while an attempt failed for a transient reason or an item is
/// deferred: 1, 2, 4, 8 minutes, capped by the configured interval.
fn retry_deadline(
    policy: &SyncPolicy,
    attempts: u32,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    policy.auto_sync_enabled.then(|| {
        let exponent = attempts.saturating_sub(1).min(4);
        let delay = 60i64
            .saturating_mul(1i64 << exponent)
            .min(i64::from(policy.sync_interval_minutes) * 60)
            .max(30);
        (now + chrono::Duration::seconds(delay)).to_rfc3339()
    })
}
fn transient_failure(code: &str) -> bool {
    matches!(
        code,
        "GITHUB_NETWORK"
            | "GITHUB_TIMEOUT"
            | "GITHUB_SERVER"
            | "GITHUB_RATE_LIMIT"
            | "GIT_TIMEOUT"
            | "GIT_READ"
            | "GIT_WRITE"
            | "GIT_FAILED"
            | "SYNC_VAULT"
    )
}
fn prune_records(config: &mut SyncConfig, now: chrono::DateTime<chrono::Utc>) {
    if config.policy.history_retention_days > 0 {
        let cutoff = now - chrono::Duration::days(config.policy.history_retention_days.into());
        config.records.retain(|record| {
            chrono::DateTime::parse_from_rfc3339(&record.completed_at)
                .is_ok_and(|time| time >= cutoff)
        });
    }
    config.records.sort_by(|a, b| {
        chrono::DateTime::parse_from_rfc3339(&b.completed_at)
            .ok()
            .cmp(&chrono::DateTime::parse_from_rfc3339(&a.completed_at).ok())
            .then_with(|| b.id.cmp(&a.id))
    });
    config.records.truncate(100);
}

fn normalize_repo(input: &str) -> Result<String> {
    let mut value = input.trim();
    for prefix in [
        "https://github.com/",
        "http://github.com/",
        "git@github.com:",
    ] {
        if let Some(rest) = value.strip_prefix(prefix) {
            value = rest;
            break;
        }
    }
    let value = value
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/');
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or("");
    let repo = parts.next().unwrap_or("");
    if parts.next().is_some() || !valid_github_part(owner) || !valid_github_part(repo) {
        return Err(err("SYNC_REPO"));
    }
    Ok(format!("{owner}/{repo}"))
}

fn parse_new_repo(input: &str) -> Result<(Option<String>, String)> {
    let mut value = input.trim();
    for prefix in [
        "https://github.com/",
        "http://github.com/",
        "git@github.com:",
    ] {
        if let Some(rest) = value.strip_prefix(prefix) {
            value = rest;
            break;
        }
    }
    let value = value
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/');
    let mut parts = value.split('/');
    let first = parts.next().unwrap_or("");
    match (parts.next(), parts.next()) {
        (Some(name), None) if valid_github_part(first) && valid_github_part(name) => {
            Ok((Some(first.to_string()), name.to_string()))
        }
        (None, None) if valid_github_part(first) => Ok((None, first.to_string())),
        _ => Err(err("SYNC_REPO")),
    }
}

fn normalize_target(provider: SyncProvider, input: &str) -> Result<String> {
    match provider {
        SyncProvider::Github => normalize_repo(input),
        SyncProvider::Git => git::validate_repo(input.trim()),
    }
}

fn valid_github_part(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 100
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
}

fn validate_file_path(path: &str) -> Result<PathBuf> {
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err(err("SYNC_PATH"));
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(err("SYNC_PATH"));
    }
    safe_file(&path)?;
    let metadata = fs::metadata(&path).map_err(|_| err("SYNC_PATH"))?;
    if !metadata.is_file() {
        return Err(err("SYNC_PATH"));
    }
    if metadata.len() > MAX_ITEM_BYTES {
        return Err(err("SYNC_FILE_TOO_LARGE"));
    }
    Ok(path)
}

fn github_status_error(status: u16) -> Error {
    let code = match status {
        401 => "GITHUB_AUTH",
        403 => "GITHUB_FORBIDDEN",
        404 => "GITHUB_REPO_NOT_FOUND",
        409 => "GITHUB_CONFLICT",
        422 => "GITHUB_REJECTED",
        429 => "GITHUB_RATE_LIMIT",
        500..=599 => "GITHUB_SERVER",
        _ => "GITHUB_REJECTED",
    };
    err(code)
}

fn map_github_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        err("GITHUB_TIMEOUT")
    } else {
        err("GITHUB_NETWORK")
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncEnvelope {
    format: String,
    kdf: String,
    salt: String,
    nonce: String,
    ciphertext: String,
}

fn encrypt_bundle(password: &str, plaintext: &[u8]) -> Result<String> {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);
    let mut key = derive_key(password, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| err("SYNC_ENCRYPT"))?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: SYNC_AAD,
            },
        )
        .map_err(|_| err("SYNC_ENCRYPT"))?;
    key.zeroize();
    serde_json::to_string(&SyncEnvelope {
        format: "ai-config-manager-sync-v1".into(),
        kdf: "argon2id".into(),
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce),
        ciphertext: STANDARD.encode(ciphertext),
    })
    .map_err(|_| err("SYNC_ENCRYPT"))
}

fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let params = Params::new(19_456, 2, 1, Some(32)).map_err(|_| err("SYNC_ENCRYPT"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|_| err("SYNC_ENCRYPT"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::sync::Arc;

    #[derive(Default)]
    struct MockSecrets(Mutex<HashMap<String, String>>);
    impl SyncSecrets for MockSecrets {
        fn get(&self, account: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }
        fn set(&self, account: &str, value: &str) -> Result<()> {
            self.0.lock().unwrap().insert(account.into(), value.into());
            Ok(())
        }
    }
    struct UnusedVault;
    impl Vault for UnusedVault {
        fn get_key(&self) -> Result<Option<SecretKey>> {
            panic!("sync test must not use the vault")
        }
        fn create_key(&self) -> Result<SecretKey> {
            panic!("sync test must not use the vault")
        }
    }

    #[test]
    fn repo_normalization_accepts_common_forms_and_rejects_paths() {
        assert_eq!(normalize_repo("owner/repo").unwrap(), "owner/repo");
        assert_eq!(
            normalize_repo("https://github.com/owner/repo.git").unwrap(),
            "owner/repo"
        );
        assert_eq!(
            normalize_repo("git@github.com:owner/repo.git").unwrap(),
            "owner/repo"
        );
        assert!(normalize_repo("../repo").is_err());
        assert!(normalize_repo("owner/repo/extra").is_err());
        assert!(normalize_repo("owner").is_err());
    }

    #[test]
    fn encryption_roundtrip_and_wrong_key() {
        let plaintext = b"secret sync payload";
        let envelope = encrypt_bundle("correct horse battery", plaintext).unwrap();
        assert!(!envelope.contains("secret sync payload"));
        let parsed: SyncEnvelope = serde_json::from_str(&envelope).unwrap();
        assert_eq!(parsed.kdf, "argon2id");
        let decrypted = decrypt_for_test("correct horse battery", &parsed).unwrap();
        assert_eq!(decrypted, plaintext);
        assert!(decrypt_for_test("wrong password", &parsed).is_err());
    }

    fn decrypt_for_test(password: &str, envelope: &SyncEnvelope) -> Result<Vec<u8>> {
        let salt = STANDARD
            .decode(&envelope.salt)
            .map_err(|_| err("SYNC_DECRYPT"))?;
        let nonce = STANDARD
            .decode(&envelope.nonce)
            .map_err(|_| err("SYNC_DECRYPT"))?;
        let ciphertext = STANDARD
            .decode(&envelope.ciphertext)
            .map_err(|_| err("SYNC_DECRYPT"))?;
        let mut key = derive_key(password, &salt)?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| err("SYNC_DECRYPT"))?;
        let decrypted = cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: SYNC_AAD,
                },
            )
            .map_err(|_| err("SYNC_DECRYPT"));
        key.zeroize();
        decrypted
    }

    #[test]
    fn items_can_be_registered_toggled_and_removed() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(
            directory.path().join("ai.yml"),
            Arc::new(UnusedVault),
        ));
        let sync = SyncStore::new(
            directory.path().join("sync.json"),
            store,
            Arc::new(MockSecrets::default()),
        );
        let file = directory.path().join("other.conf");
        fs::write(&file, "other=true\n").unwrap();
        let view = sync
            .add_item(Some("Other config".into()), file.to_str().unwrap(), true)
            .unwrap();
        assert_eq!(view.items.len(), 2);
        let id = view
            .items
            .iter()
            .find(|item| item.kind == SyncKind::File)
            .unwrap()
            .id
            .clone();
        let view = sync.set_item_enabled(&id, false).unwrap();
        assert!(
            !view
                .items
                .iter()
                .find(|item| item.id == id)
                .unwrap()
                .enabled
        );
        let view = sync.remove_item(&id).unwrap();
        assert_eq!(view.items.len(), 1);
        assert!(sync.remove_item(AI_CONFIG_ITEM_ID).is_err());
    }

    #[test]
    fn invalid_settings_and_failed_persistence_keep_the_saved_state() {
        let directory = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MockSecrets::default());
        let store = Arc::new(Store::new(
            directory.path().join("ai.yml"),
            Arc::new(UnusedVault),
        ));
        let path = directory.path().join("blocked").join("sync.json");
        let sync = SyncStore::new(path, store, secrets.clone());
        assert_eq!(
            sync.save_settings(
                "alice/configs",
                Some("new-token".into()),
                Some("short".into())
            )
            .unwrap_err()
            .code,
            "SYNC_PASSWORD"
        );
        assert!(secrets.0.lock().unwrap().is_empty());
        fs::write(directory.path().join("blocked"), "not a directory").unwrap();
        let file = directory.path().join("other.conf");
        fs::write(&file, "other=true").unwrap();
        assert!(sync.add_item(None, file.to_str().unwrap(), true).is_err());
        assert_eq!(sync.view().items.len(), 1);
        assert!(sync.save_settings("alice/configs", None, None).is_err());
        assert!(sync.view().repo.is_empty());
        assert!(sync.set_item_enabled(AI_CONFIG_ITEM_ID, false).is_err());
        assert!(sync.view().items[0].enabled);
    }

    fn setup_sync() -> (tempfile::TempDir, Arc<Store>, SyncStore) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let store = Arc::new(Store::new(root.join("ai.yml"), Arc::new(UnusedVault)));
        let sync = SyncStore::new(
            root.join("sync.json"),
            store.clone(),
            Arc::new(MockSecrets::default()),
        );
        (directory, store, sync)
    }

    #[test]
    fn saved_ai_config_requests_a_debounced_scheduled_sync() {
        let (_directory, store, sync) = setup_sync();
        sync.save_settings(
            "example/configs",
            Some("synthetic-token".into()),
            Some("synthetic-encryption-secret".into()),
        )
        .unwrap();
        store
            .save(SaveRequest {
                base_url: "https://example.com/v1".into(),
                model: "test-model".into(),
                api_key: Some("synthetic-key".into()),
                encrypt: false,
                expected_revision: "missing".into(),
            })
            .unwrap();
        assert!(store.has_dirty_flag());
        assert!(sync.poll_ai_change().unwrap());
        assert!(!store.has_dirty_flag());
        assert!(!sync.poll_ai_change().unwrap());
        let due =
            chrono::DateTime::parse_from_rfc3339(sync.view().next_sync_at.as_deref().unwrap())
                .unwrap();
        assert!(due <= chrono::Utc::now() + chrono::Duration::seconds(3));
        let pending = sync.state.lock().unwrap().pending_sync;
        assert!(pending);
    }

    #[test]
    fn disabled_auto_sync_or_ai_item_keeps_the_change_pending() {
        let (_directory, store, sync) = setup_sync();
        sync.save_policy(SyncPolicy {
            auto_sync_enabled: false,
            sync_interval_minutes: 10,
            history_retention_days: 30,
        })
        .unwrap();
        store
            .save(SaveRequest {
                base_url: "https://example.com/v1".into(),
                model: "test-model".into(),
                api_key: Some("synthetic-key".into()),
                encrypt: false,
                expected_revision: "missing".into(),
            })
            .unwrap();
        assert!(!sync.poll_ai_change().unwrap());
        assert!(store.has_dirty_flag());
        sync.save_settings(
            "example/configs",
            Some("synthetic-token".into()),
            Some("synthetic-encryption-secret".into()),
        )
        .unwrap();
        sync.save_policy(SyncPolicy {
            auto_sync_enabled: true,
            sync_interval_minutes: 10,
            history_retention_days: 30,
        })
        .unwrap();
        sync.set_item_enabled("ai-config", false).unwrap();
        assert!(!sync.poll_ai_change().unwrap());
        assert!(store.has_dirty_flag());
        sync.set_item_enabled("ai-config", true).unwrap();
        assert!(sync.poll_ai_change().unwrap());
        assert!(!store.has_dirty_flag());
    }

    #[test]
    fn a_change_during_an_active_sync_stays_pending() {
        let (_directory, _store, sync) = setup_sync();
        sync.save_settings(
            "example/configs",
            Some("synthetic-token".into()),
            Some("synthetic-encryption-secret".into()),
        )
        .unwrap();
        let _running = sync.operation.lock().unwrap();
        assert!(sync.request_sync_after_config_change().unwrap());
        let state = sync.state.lock().unwrap();
        assert!(state.pending_sync);
        assert!(
            chrono::DateTime::parse_from_rfc3339(state.next_sync_at.as_deref().unwrap()).unwrap()
                <= chrono::Utc::now() + chrono::Duration::seconds(3)
        );
    }

    #[test]
    fn legacy_settings_keep_github_defaults_and_git_target_survives_restart() {
        let (_directory, store, sync) = setup_sync();
        sync.save_settings(
            "example/configs",
            Some("synthetic-token".into()),
            Some("synthetic-encryption-secret".into()),
        )
        .unwrap();
        let mut legacy: JsonValue = serde_json::from_slice(&fs::read(&sync.path).unwrap()).unwrap();
        legacy.as_object_mut().unwrap().remove("provider");
        legacy.as_object_mut().unwrap().remove("git_branch");
        fs::write(&sync.path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let secrets = sync.secrets.clone();
        let restarted = SyncStore::new(sync.path.clone(), store.clone(), secrets.clone());
        assert_eq!(restarted.view().provider, SyncProvider::Github);
        assert_eq!(restarted.view().git_branch, "main");
        assert!(restarted.view().token_present);
        restarted
            .save_repository_settings(
                SyncProvider::Git,
                "ssh://git@example.com/configs.git",
                "team/settings",
                None,
                None,
            )
            .unwrap();
        let restarted = SyncStore::new(sync.path.clone(), store, secrets);
        let view = restarted.view();
        assert_eq!(view.provider, SyncProvider::Git);
        assert_eq!(view.git_branch, "team/settings");
        assert!(!view.token_present);
        assert!(view.encryption_secret_present);
        assert_eq!(
            restarted
                .save_repository_settings(SyncProvider::Git, "ext::unsafe", "main", None, None)
                .unwrap_err()
                .code,
            "GIT_REPO"
        );
        assert_eq!(
            restarted
                .save_repository_settings(SyncProvider::Git, &view.repo, "../bad", None, None)
                .unwrap_err()
                .code,
            "GIT_BRANCH"
        );
        assert_eq!(restarted.view().git_branch, "team/settings");
        let bytes = fs::read_to_string(&sync.path).unwrap();
        assert!(!bytes.contains("synthetic-"));
    }

    #[test]
    fn application_registration_is_stable_idempotent_and_validated() {
        let (_directory, _store, sync) = setup_sync();
        let request = ApplicationRegistration {
            app_id: "editor".into(),
            app_name: "My Editor".into(),
            config_id: "preferences".into(),
            label: "Preferences".into(),
            enabled: true,
            sync_interface: SyncInterface {
                socket_path: sync
                    .path
                    .with_file_name("editor.sock")
                    .to_str()
                    .unwrap()
                    .into(),
                path: "/config".into(),
            },
        };
        let first = sync.register_application(request.clone()).unwrap();
        assert!(first.items[1].auto_sync);
        let deadline = first.next_sync_at.clone();
        sync.set_item_enabled(&first.items[1].id, false).unwrap();
        sync.set_application_auto_sync("editor", false).unwrap();
        let mut renamed = request.clone();
        renamed.app_name = "Editor 2".into();
        let updated = sync.register_application(renamed).unwrap();
        assert_eq!(updated.items.len(), 2);
        assert_eq!(first.items[1].id, updated.items[1].id);
        assert!(!updated.items[1].enabled);
        assert!(!updated.items[1].auto_sync);
        assert_eq!(updated.next_sync_at, deadline);
        assert_eq!(
            sync.run_application("editor", SyncTrigger::Agent)
                .unwrap_err()
                .code,
            "SYNC_DISABLED"
        );
        assert_eq!(
            sync.run_application("missing", SyncTrigger::Agent)
                .unwrap_err()
                .code,
            "SYNC_ITEM_MISSING"
        );
        assert_eq!(
            updated.items[1].application.as_ref().unwrap().app_name,
            "Editor 2"
        );
        let mut invalid = request;
        invalid.app_id = "../unsafe".into();
        assert_eq!(
            sync.register_application(invalid).unwrap_err().code,
            "SYNC_APPLICATION"
        );
        let reloaded = SyncStore::new(
            sync.path.clone(),
            sync.store.clone(),
            Arc::new(MockSecrets::default()),
        );
        assert_eq!(reloaded.view().items.len(), 2);
        assert!(!reloaded.view().items[1].auto_sync);
        assert!(!reloaded.view().items[1].enabled);
        assert_eq!(
            reloaded.view().items[1].application,
            updated.items[1].application
        );
    }

    #[test]
    fn legacy_items_default_to_automatic_and_failed_scoped_sync_preserves_other_retries() {
        let (_directory, store, sync) = setup_sync();
        sync.register_application(ApplicationRegistration {
            app_id: "editor".into(),
            app_name: "Editor".into(),
            config_id: "settings".into(),
            label: "Settings".into(),
            enabled: true,
            sync_interface: SyncInterface {
                socket_path: sync
                    .path
                    .with_file_name("editor.sock")
                    .to_str()
                    .unwrap()
                    .into(),
                path: "/config".into(),
            },
        })
        .unwrap();
        let other = DeferredItem {
            id: AI_CONFIG_ITEM_ID.into(),
            label: "AI config".into(),
            kind: SyncKind::AiConfig,
            reason: "SYNC_APPLICATION_OFFLINE".into(),
        };
        {
            let mut state = sync.lock_state().unwrap();
            state.deferred.push(other.clone());
            state.retry_attempts = 3;
            sync.save_locked(&state).unwrap();
        }
        let deadline = sync.view().next_sync_at;
        assert_eq!(
            sync.run_application("editor", SyncTrigger::Agent)
                .unwrap_err()
                .code,
            "SYNC_NOT_CONFIGURED"
        );
        assert_eq!(sync.view().deferred, vec![other]);
        assert_eq!(sync.view().next_sync_at, deadline);
        assert_eq!(sync.lock_state().unwrap().retry_attempts, 3);
        assert_eq!(sync.view().records[0].app_id.as_deref(), Some("editor"));
        let mut legacy: JsonValue = serde_json::from_slice(&fs::read(&sync.path).unwrap()).unwrap();
        for item in legacy["items"].as_array_mut().unwrap() {
            item.as_object_mut().unwrap().remove("auto_sync");
        }
        for record in legacy["records"].as_array_mut().unwrap() {
            record.as_object_mut().unwrap().remove("app_id");
        }
        fs::write(&sync.path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let restarted = SyncStore::new(sync.path.clone(), store, sync.secrets.clone());
        assert!(restarted.view().items.iter().all(|item| item.auto_sync));
        assert_eq!(restarted.view().records[0].app_id, None);
    }

    #[test]
    fn default_policy_enables_auto_sync_every_ten_minutes() {
        let (_directory, _store, sync) = setup_sync();
        let default = SyncPolicy {
            auto_sync_enabled: true,
            sync_interval_minutes: 10,
            history_retention_days: 30,
        };
        assert_eq!(SyncPolicy::default(), default);
        assert_eq!(sync.view().policy, default);
        let now = chrono::Utc::now();
        // The first tick schedules the deadline; nothing runs without a target.
        assert!(sync.run_due_sync(now).unwrap().is_none());
        assert_eq!(
            sync.view().next_sync_at,
            Some((now + chrono::Duration::minutes(10)).to_rfc3339())
        );
        assert!(sync.view().records.is_empty());
    }

    #[test]
    fn scheduled_sync_skips_without_a_repository_and_records_failures_once() {
        let (_directory, _store, sync) = setup_sync();
        assert!(sync.run_scheduled().unwrap().is_none());
        let enabled = SyncPolicy {
            auto_sync_enabled: true,
            sync_interval_minutes: 5,
            history_retention_days: 30,
        };
        sync.save_policy(enabled.clone()).unwrap();
        let due = chrono::DateTime::parse_from_rfc3339(sync.view().next_sync_at.as_ref().unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(sync
            .run_due_sync(due - chrono::Duration::seconds(1))
            .unwrap()
            .is_none());
        // No target yet: keep the schedule moving without recording failures.
        assert!(sync.run_due_sync(due).unwrap().is_none());
        let view = sync.view();
        assert!(view.records.is_empty());
        assert_eq!(
            view.next_sync_at,
            Some((due + chrono::Duration::minutes(5)).to_rfc3339())
        );

        // A saved but unreachable target records one failed attempt and still
        // advances the deadline.
        let sync = sync.with_api_base("notaurl".into());
        sync.save_settings(
            "alice/configs",
            Some("synthetic-token".into()),
            Some("correct horse battery".into()),
        )
        .unwrap();
        let due = chrono::DateTime::parse_from_rfc3339(sync.view().next_sync_at.as_ref().unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(sync.run_due_sync(due).unwrap_err().code, "GITHUB_NETWORK");
        assert!(sync.run_due_sync(due).unwrap().is_none());
        let view = sync.view();
        assert_eq!(view.records.len(), 1);
        assert_eq!(view.records[0].trigger, SyncTrigger::Scheduled);
        assert_eq!(
            view.records[0].error_code.as_deref(),
            Some("GITHUB_NETWORK")
        );
        // A transient failure schedules a short retry instead of the full interval.
        let first_retry = due + chrono::Duration::seconds(60);
        assert_eq!(view.next_sync_at, Some(first_retry.to_rfc3339()));
        assert_eq!(
            sync.run_due_sync(first_retry).unwrap_err().code,
            "GITHUB_NETWORK"
        );
        // Consecutive transient failures back off before the next attempt.
        assert_eq!(
            sync.view().next_sync_at,
            Some((first_retry + chrono::Duration::seconds(120)).to_rfc3339())
        );
        let restarted = SyncStore::new(
            sync.path.clone(),
            sync.store.clone(),
            Arc::new(MockSecrets::default()),
        );
        assert!(restarted.run_due_sync(due).unwrap().is_none());
        assert_eq!(restarted.view().records.len(), 2);
        let mut invalid = enabled.clone();
        invalid.sync_interval_minutes = 0;
        assert_eq!(sync.save_policy(invalid).unwrap_err().code, "SYNC_POLICY");
        assert_eq!(sync.view().policy, enabled);
        sync.save_policy(SyncPolicy {
            auto_sync_enabled: false,
            sync_interval_minutes: 10,
            history_retention_days: 30,
        })
        .unwrap();
        assert!(sync.view().next_sync_at.is_none());
        assert!(sync
            .run_due_sync(due + chrono::Duration::days(10))
            .unwrap()
            .is_none());
    }

    #[test]
    fn startup_and_completed_attempts_delete_records_beyond_one_hundred() {
        let (_directory, _store, sync) = setup_sync();
        sync.run_sync().unwrap_err();
        let sample = sync.view().records[0].clone();
        let now = chrono::Utc::now();
        {
            let mut state = sync.lock_state().unwrap();
            state.policy.history_retention_days = 0;
            state.records = (0..105)
                .map(|index| {
                    let mut record = sample.clone();
                    record.id = format!("{index:032x}");
                    record.completed_at = (now - chrono::Duration::seconds(index)).to_rfc3339();
                    record
                })
                .collect();
            sync.save_locked(&state).unwrap();
        }
        let restarted = SyncStore::new(
            sync.path.clone(),
            sync.store.clone(),
            Arc::new(MockSecrets::default()),
        );
        assert_eq!(restarted.view().records.len(), 100);
        let bytes = fs::read(&sync.path).unwrap();
        let persisted: SyncConfig = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(persisted.records.len(), 100);
        assert_eq!(persisted.records.last().unwrap().id, format!("{:032x}", 99));
        restarted.run_sync().unwrap_err();
        assert_eq!(restarted.history().unwrap().len(), 100);
        assert_eq!(restarted.view().records[0].trigger, SyncTrigger::Manual);
    }

    #[test]
    fn age_cleanup_prunes_sync_and_config_history_without_changing_config() {
        let (_directory, store, sync) = setup_sync();
        store
            .save(SaveRequest {
                base_url: "https://example.com/v1".into(),
                model: "model".into(),
                api_key: Some("synthetic-key".into()),
                encrypt: false,
                expected_revision: "missing".into(),
            })
            .unwrap();
        let original = fs::read(&store.path).unwrap();
        let now = chrono::Utc::now();
        let mut history = store.history().unwrap();
        history[0].timestamp = (now - chrono::Duration::days(60)).to_rfc3339();
        fs::write(
            store.side(".history.json"),
            serde_json::to_vec(&history).unwrap(),
        )
        .unwrap();
        sync.run_sync().unwrap_err();
        {
            let mut state = sync.lock_state().unwrap();
            let mut expired = state.records[0].clone();
            expired.id = random_id();
            expired.completed_at = history[0].timestamp.clone();
            state.records.push(expired);
            sync.save_locked(&state).unwrap();
        }
        sync.cleanup_at(now).unwrap();
        assert_eq!(sync.view().records.len(), 1);
        assert!(store.history().unwrap().is_empty());
        assert_eq!(fs::read(&store.path).unwrap(), original);
        let persisted: SyncConfig = serde_json::from_slice(&fs::read(&sync.path).unwrap()).unwrap();
        assert_eq!(persisted.records.len(), 1);
    }

    #[test]
    fn interrupted_attempt_is_recorded_only_once_on_restart() {
        let (_directory, _store, sync) = setup_sync();
        sync.run_sync().unwrap_err();
        {
            let mut state = sync.lock_state().unwrap();
            state.active_record = state.records.pop();
            state.status.state = SyncState::Running;
            sync.save_locked(&state).unwrap();
        }
        for _ in 0..2 {
            let restarted = SyncStore::new(
                sync.path.clone(),
                sync.store.clone(),
                Arc::new(MockSecrets::default()),
            );
            assert_eq!(restarted.view().records.len(), 1);
            assert_eq!(
                restarted.view().records[0].error_code.as_deref(),
                Some("SYNC_INTERRUPTED")
            );
            assert_eq!(restarted.view().status.state, SyncState::Error);
        }
    }

    #[test]
    fn sync_pushes_an_encrypted_bundle_to_github_contents_api() {
        use std::net::TcpListener;
        use std::thread;

        std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
        std::env::set_var("no_proxy", "127.0.0.1,localhost");
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(
            directory.path().join("ai.yml"),
            Arc::new(UnusedVault),
        ));
        store
            .save(SaveRequest {
                base_url: "https://example.com/v1".into(),
                model: "model".into(),
                api_key: Some("portable-secret".into()),
                encrypt: false,
                expected_revision: "missing".into(),
            })
            .unwrap();
        let sync_path = directory.path().join("sync.json");
        let sync = SyncStore::new(sync_path.clone(), store, Arc::new(MockSecrets::default()));
        sync.save_settings(
            "alice/configs",
            Some("test-token".into()),
            Some("correct horse battery".into()),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let sync = Arc::new(sync.with_api_base(format!("http://{address}")));
        let server_sync = sync.clone();
        let third_party = directory.path().join("third-party.json");
        fs::write(&third_party, r#"{"theme":"dark"}"#).unwrap();
        sync.add_item(
            Some("Third party".into()),
            third_party.to_str().unwrap(),
            true,
        )
        .unwrap();
        let server = thread::spawn(move || {
            let mut remote_content = String::new();
            for attempt in 0..2 {
                let (mut get, _) = listener.accept().unwrap();
                let get_request = read_http_request(&mut get);
                assert!(get_request.starts_with("GET /repos/alice/configs/contents/"));
                assert_eq!(
                    server_sync
                        .set_item_enabled(AI_CONFIG_ITEM_ID, false)
                        .unwrap_err()
                        .code,
                    "SYNC_BUSY"
                );
                assert_eq!(
                    server_sync
                        .save_settings("other/repo", None, None)
                        .unwrap_err()
                        .code,
                    "SYNC_BUSY"
                );
                assert_eq!(server_sync.run_sync().unwrap_err().code, "SYNC_BUSY");
                if attempt == 0 {
                    write_http_response(&mut get, 404, "");
                } else {
                    write_http_response(
                        &mut get,
                        200,
                        &json!({"sha":"remote-sha", "content": remote_content}).to_string(),
                    );
                }
                let (mut put, _) = listener.accept().unwrap();
                let put_request = read_http_request(&mut put);
                assert!(put_request.starts_with("PUT /repos/alice/configs/contents/"));
                assert!(put_request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer test-token"));
                assert!(!put_request.contains("portable-secret"));
                assert!(!put_request.contains("correct horse battery"));
                let body: JsonValue =
                    serde_json::from_str(put_request.split_once("\r\n\r\n").unwrap().1).unwrap();
                remote_content = body["content"].as_str().unwrap().into();
                if attempt == 0 {
                    assert!(body.get("sha").is_none());
                } else {
                    assert_eq!(body["sha"], "remote-sha");
                }
                let envelope = STANDARD.decode(body["content"].as_str().unwrap()).unwrap();
                let envelope: SyncEnvelope = serde_json::from_slice(&envelope).unwrap();
                let plaintext = decrypt_for_test("correct horse battery", &envelope).unwrap();
                let bundle: JsonValue = serde_json::from_slice(&plaintext).unwrap();
                let items = bundle["items"].as_array().unwrap();
                assert_eq!(items.len(), 2);
                let ai = items
                    .iter()
                    .find(|item| item["kind"] == "ai_config")
                    .unwrap();
                let file = items.iter().find(|item| item["kind"] == "file").unwrap();
                let ai_yaml = STANDARD.decode(ai["content"].as_str().unwrap()).unwrap();
                assert!(String::from_utf8(ai_yaml)
                    .unwrap()
                    .contains("portable-secret"));
                let content = STANDARD.decode(file["content"].as_str().unwrap()).unwrap();
                assert_eq!(
                    content,
                    if attempt == 0 {
                        br#"{"theme":"dark"}"#.as_slice()
                    } else {
                        br#"{"theme":"light"}"#.as_slice()
                    }
                );
                write_http_response(
                    &mut put,
                    200,
                    r#"{"content":{"sha":"remote-sha","html_url":"https://example.test/sync.enc"}}"#,
                );
            }
            let (mut get, _) = listener.accept().unwrap();
            read_http_request(&mut get);
            write_http_response(&mut get, 401, "do not expose this response body");
        });
        let view = sync.run_sync().unwrap();
        assert_eq!(view.status.state, SyncState::Success);
        assert_eq!(view.status.item_count, 2);
        assert_eq!(view.status.remote_sha.as_deref(), Some("remote-sha"));
        assert_eq!(
            view.status.remote_url.as_deref(),
            Some("https://example.test/sync.enc")
        );
        fs::write(&third_party, r#"{"theme":"light"}"#).unwrap();
        assert_eq!(sync.run_sync().unwrap().status.state, SyncState::Success);
        assert_eq!(sync.run_sync().unwrap_err().code, "GITHUB_AUTH");
        assert_eq!(sync.view().status.state, SyncState::Error);
        assert!(sync.view().status.last_synced_at.is_some());
        assert!(!sync.view().status.message.contains("do not expose"));
        let persisted = fs::read_to_string(sync_path).unwrap();
        assert!(!persisted.contains("test-token"));
        assert!(!persisted.contains("correct horse battery"));
        server.join().unwrap();
    }

    #[test]
    fn creates_private_github_repositories_without_saving_settings() {
        use std::net::TcpListener;
        use std::thread;

        std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
        std::env::set_var("no_proxy", "127.0.0.1,localhost");
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(
            directory.path().join("ai.yml"),
            Arc::new(UnusedVault),
        ));
        let sync = SyncStore::new(
            directory.path().join("sync.json"),
            store,
            Arc::new(MockSecrets::default()),
        );
        assert_eq!(
            sync.create_github_repository(CreateRepositoryRequest {
                repo: "alice/configs".into(),
                github_token: None,
            })
            .unwrap_err()
            .code,
            "SYNC_NOT_CONFIGURED"
        );
        assert_eq!(
            sync.create_github_repository(CreateRepositoryRequest {
                repo: "../escape".into(),
                github_token: Some("draft-token".into()),
            })
            .unwrap_err()
            .code,
            "SYNC_REPO"
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let sync = sync.with_api_base(format!("http://{address}"));
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("GET /user "), "{request}");
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer draft-token"));
            write_http_response(&mut stream, 200, r#"{"login":"alice"}"#);

            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /user/repos "), "{request}");
            let body: JsonValue =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["name"], "configs");
            assert_eq!(body["private"], true);
            write_http_response(
                &mut stream,
                201,
                r#"{"full_name":"alice/configs","html_url":"https://github.com/alice/configs","private":true}"#,
            );

            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("GET /user "));
            write_http_response(&mut stream, 200, r#"{"login":"alice"}"#);

            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /orgs/team/repos "));
            let body: JsonValue =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["name"], "settings");
            write_http_response(
                &mut stream,
                201,
                r#"{"full_name":"team/settings","html_url":"https://github.com/team/settings","private":true}"#,
            );

            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("GET /user "));
            write_http_response(&mut stream, 200, r#"{"login":"alice"}"#);

            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /user/repos "));
            write_http_response(
                &mut stream,
                422,
                r#"{"message":"Repository creation failed."}"#,
            );
        });
        let created = sync
            .create_github_repository(CreateRepositoryRequest {
                repo: "https://github.com/alice/configs.git".into(),
                github_token: Some("draft-token".into()),
            })
            .unwrap();
        assert_eq!(created.repo, "alice/configs");
        assert_eq!(created.url, "https://github.com/alice/configs");
        assert!(created.private);
        let organization = sync
            .create_github_repository(CreateRepositoryRequest {
                repo: "team/settings".into(),
                github_token: Some("draft-token".into()),
            })
            .unwrap();
        assert_eq!(organization.repo, "team/settings");
        assert_eq!(
            sync.create_github_repository(CreateRepositoryRequest {
                repo: "alice/taken".into(),
                github_token: Some("draft-token".into()),
            })
            .unwrap_err()
            .code,
            "GITHUB_REJECTED"
        );
        server.join().unwrap();
        assert!(sync.view().repo.is_empty());
        assert!(!sync.view().token_present);
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        request.push_str(&line);
        let mut content_length = 0usize;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            request.push_str(&line);
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).unwrap();
        request.push_str(&String::from_utf8_lossy(&body));
        request
    }

    fn write_http_response(stream: &mut std::net::TcpStream, status: u16, body: &str) {
        let reason = match status {
            200 => "OK",
            201 => "Created",
            404 => "Not Found",
            422 => "Unprocessable Entity",
            _ => "Error",
        };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        stream.flush().unwrap();
    }

    #[test]
    fn portable_export_contains_plaintext_for_sync_encryption_only() {
        // The sync bundle is encrypted by encrypt_bundle before it leaves this
        // process. This test only verifies that export_portable produces the
        // portable representation needed for another machine to restore it.
        let directory = tempfile::tempdir().unwrap();
        let store = Store::new(directory.path().join("ai.yml"), Arc::new(UnusedVault));
        let request = SaveRequest {
            base_url: "https://example.com/v1".into(),
            model: "model".into(),
            api_key: Some("portable-secret".into()),
            encrypt: false,
            expected_revision: "missing".into(),
        };
        store.save(request).unwrap();
        let exported = store.export_portable().unwrap();
        assert!(exported.contains("portable-secret"));
    }
}
