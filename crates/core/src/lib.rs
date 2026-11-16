//! Credential-safe configuration storage. No command should serialize Secret or ConfigSnapshot.
use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use fs2::FileExt;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};
pub mod agent;
mod config;
mod crypto;
mod filesystem;
mod history;
mod platform;
mod probe;
pub mod rest;
pub mod sync;
mod sync_application;
use config::*;
pub use crypto::{NativeVault, Secret, SecretKey, Vault};
use filesystem::*;
use history::*;
pub use probe::ProbeResult;
const MAX_FILE: u64 = 2 * 1024 * 1024;
const AAD: &[u8] = b"ai-config-manager:api-key:v1";
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Error {
    pub code: String,
    pub message: String,
}
fn err(code: &str) -> Error {
    Error {
        code: code.into(),
        message: match code {
            "IO" => "Storage could not be read or written. Check access permissions and available disk space, then reload before retrying.",
            "LOCK_TIMEOUT" => "Another process is using this configuration. Wait for it to finish, then retry.",
            "HISTORY_PENDING" => "Configuration was replaced, but history finalization or durability confirmation is pending. Reload configuration and read history to recover before another save.",
            "COMMIT_UNCERTAIN" => "The save outcome could not be verified. Reload and inspect the configuration before retrying; do not assume it was unchanged.",
            "ENCRYPT_FAILED" => "Credential encryption failed. The configuration was not replaced; retry after checking the credential vault.",
            "INVALID_SOURCE" => "The history source must be desktop, cli, skill, or test.",
            "CONFLICT" => "Configuration changed; reload before saving.",
            "VAULT_UNAVAILABLE" => "The system credential vault is unavailable or locked.",
            "KEY_MISSING" => "The encryption key is missing from the credential vault.",
            "HISTORY_CORRUPT" => "History requires recovery before saving.",
            "UNSAFE_PATH" => "A configuration path is not a safe regular file or directory.",
            "INVALID_CONFIG" => "Configuration format is invalid or ambiguous.",
            "INVALID_URL" => {
                "Use an HTTP or HTTPS base URL without credentials, query, or fragment."
            }
            "INVALID_MODEL" => "A nonempty model is required.",
            "INVALID_KEY" => "A nonempty API key is required.",
            "DECRYPT_FAILED" => "The saved credential could not be decrypted.",
            "TOO_LARGE" => "Configuration exceeds the supported size limit.",
            "AGENT_UNAVAILABLE" => {
                "The desktop agent interface is unavailable; direct configuration access can still be used."
            }
            "AGENT_IN_USE" => "Another desktop agent is already listening on the local socket.",
            "AGENT_PROTOCOL" => "The local agent request or response was not valid.",
            "AGENT_TOKEN" => "The local agent token is missing, malformed, or unreadable.",
            "UNAUTHORIZED" => "The local agent rejected the request token.",
            "INVALID_ARGUMENT" => "The local agent request is malformed.",
            "SYNC_CONFIG" => "Sync settings are invalid or unsupported; repair or remove the sync settings file.",
            "SYNC_STATE" => "Sync state is busy or unavailable.",
            "SYNC_VAULT" => "The sync credential vault is unavailable.",
            "SYNC_REPO" => "Enter a GitHub repository as owner/repo or a github.com URL.",
            "SYNC_TOKEN" => "A nonempty GitHub token is required.",
            "SYNC_PASSWORD" => "The sync encryption password must be at least 8 characters.",
            "SYNC_NOT_CONFIGURED" => "Configure the GitHub repository, token, and encryption password before syncing.",
            "SYNC_BUSY" => "A sync is already running.",
            "SYNC_PATH" => "Use an absolute path to a readable regular file.",
            "SYNC_FILE_TOO_LARGE" => "A sync item or bundle exceeds the supported size limit.",
            "SYNC_DUPLICATE" => "That file is already registered for sync.",
            "SYNC_ITEM_MISSING" => "The sync item was not found.",
            "SYNC_REQUIRED" => "The AI configuration item cannot be removed.",
            "SYNC_DISABLED" => "Enable this application's configuration before syncing.",
            "SYNC_BUNDLE" => "The sync bundle could not be encoded.",
            "SYNC_ENCRYPT" => "The sync bundle could not be encrypted.",
            "SYNC_DECRYPT" => "The sync bundle could not be decrypted.",
            "GIT_UNAVAILABLE" => "Install Git and make it available to the application.",
            "GIT_REPO" => "Use an HTTPS, SSH or absolute local Git repository without embedded credentials.",
            "GIT_BRANCH" => "Use a valid Git branch name.",
            "GIT_READ" => "The Git repository could not be read with the current credentials.",
            "GIT_WRITE" => "The Git repository rejected the write.",
            "GIT_CONFLICT" => "The Git branch changed during sync; refresh and merge again.",
            "GIT_TIMEOUT" => "The Git operation timed out.",
            "GIT_CLEANUP" => "The temporary repository test branch could not be removed.",
            "GIT_FAILED" => "The Git operation failed.",
            "SYNC_INTERFACE" => "Use an absolute local socket path and a valid HTTP callback path.",
            "SYNC_CALLBACK" => "The application sync interface is unavailable or returned an invalid response.",
            "SYNC_APPLICATION" => "Use stable application and configuration IDs and a nonempty application name.",
            "SYNC_POLICY" => "Use a sync interval of 1 to 1440 minutes and retention of 0 to 3650 days.",
            "SYNC_LOCAL_CHANGED" => "The local configuration changed during sync; it was not overwritten.",
            "SYNC_INTERRUPTED" => "The previous sync was interrupted before its result was recorded.",
            "GITHUB_AUTH" => "GitHub rejected the token.",
            "GITHUB_FORBIDDEN" => "GitHub denied access; the token may lack repository contents permission.",
            "GITHUB_REPO_NOT_FOUND" => "The GitHub repository or sync path was not found.",
            "GITHUB_CONFLICT" => "The remote sync file changed; retry the sync.",
            "GITHUB_REJECTED" => "GitHub rejected the sync request.",
            "GITHUB_SERVER" => "GitHub returned a server error.",
            "GITHUB_NETWORK" => "GitHub could not be reached.",
            "GITHUB_TIMEOUT" => "The GitHub request timed out.",
            _ => "The configuration operation could not be completed.",
        }
        .into(),
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigView {
    pub exists: bool,
    pub base_url: String,
    pub model: String,
    pub key_present: bool,
    pub encrypted: bool,
    pub revision: String,
    #[serde(default)]
    pub endpoint: EndpointKind,
    /// Whether this endpoint's block exists. `exists` still describes the file.
    #[serde(default)]
    pub configured: bool,
}
/// Independent model endpoints stored under `ai.<kind>`. Only the OpenAI
/// endpoint requires a credential; local gateways commonly accept none.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EndpointKind {
    #[default]
    Openai,
    Embedding,
    Laya,
    Cosyvoice,
}
impl EndpointKind {
    pub const ALL: [Self; 4] = [Self::Openai, Self::Embedding, Self::Laya, Self::Cosyvoice];
    pub const fn key(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Embedding => "embedding",
            Self::Laya => "laya",
            Self::Cosyvoice => "cosyvoice",
        }
    }
    pub const fn requires_key(self) -> bool {
        matches!(self, Self::Openai)
    }
    pub fn from_key(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.key() == value)
    }
}
fn default_true() -> bool {
    true
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveRequest {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Encryption is the safe default for every caller that omits the field.
    /// Explicit plaintext still requires an explicit `false` (the GUI and CLI
    /// both make the user opt in).
    #[serde(default = "default_true")]
    pub encrypt: bool,
    pub expected_revision: String,
}
impl std::fmt::Debug for SaveRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveRequest")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("encrypt", &self.encrypt)
            .finish()
    }
}
impl Drop for SaveRequest {
    fn drop(&mut self) {
        if let Some(k) = self.api_key.as_mut() {
            k.zeroize()
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDiff {
    pub field: String,
    pub before: String,
    pub after: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryEntry {
    pub id: String,
    pub timestamp: String,
    pub diff: Vec<FieldDiff>,
    pub sha256: String,
    pub source: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    before_revision: String,
    after_revision: String,
    before_history_revision: String,
    history: Vec<HistoryEntry>,
}
pub struct ConfigSnapshot {
    pub endpoint: EndpointKind,
    pub base_url: String,
    pub model: String,
    pub encrypted: bool,
    secret: Secret,
}
impl ConfigSnapshot {
    pub fn with_secret<T>(&self, f: impl FnOnce(&str) -> T) -> T {
        self.secret.expose(f)
    }
}
pub struct Store {
    path: PathBuf,
    vault: Arc<dyn Vault>,
}
impl Store {
    pub fn new(path: PathBuf, vault: Arc<dyn Vault>) -> Self {
        let path = if path.is_relative() {
            std::env::current_dir()
                .map(|dir| dir.join(&path))
                .unwrap_or(path)
        } else {
            path
        };
        Self { path, vault }
    }
    pub fn default_path() -> Result<PathBuf> {
        dirs::home_dir()
            .map(|p| p.join(".config/ai.yml"))
            .ok_or_else(|| err("IO"))
    }
    fn side(&self, suffix: &str) -> PathBuf {
        let mut s = self.path.as_os_str().to_os_string();
        s.push(suffix);
        PathBuf::from(s)
    }
    fn lock(&self) -> Result<File> {
        let parent = self.path.parent().ok_or_else(|| err("UNSAFE_PATH"))?;
        safe_ancestors(parent)?;
        fs::create_dir_all(parent).map_err(|_| err("IO"))?;
        safe_ancestors(parent)?;
        let p = self.side(".lock");
        safe_file(&p)?;
        let f = platform::open_private(&p, false)?;
        lock_bounded(&f)?;
        Ok(f)
    }
    fn document(&self) -> Result<(Value, String, bool)> {
        match read_bounded(&self.path)? {
            None => Ok((Value::Mapping(Mapping::new()), "missing".into(), false)),
            Some(raw) => {
                let revision = hash(&raw);
                let text =
                    Zeroizing::new(String::from_utf8(raw).map_err(|_| err("INVALID_CONFIG"))?);
                let value = parse(&text)?;
                Ok((value, revision, true))
            }
        }
    }
    /// Metadata for the OpenAI endpoint, kept as the default for old callers.
    pub fn load(&self) -> Result<ConfigView> {
        self.load_endpoint(EndpointKind::Openai)
    }
    pub fn load_endpoint(&self, kind: EndpointKind) -> Result<ConfigView> {
        let _lock = self.lock()?;
        let (doc, revision, exists) = self.document()?;
        view(&doc, kind, revision, exists)
    }
    /// Existing endpoints must be configured. Missing ones fail instead of
    /// silently borrowing the OpenAI address or credential.
    fn required_view(
        &self,
        doc: &Value,
        kind: EndpointKind,
        revision: String,
        exists: bool,
    ) -> Result<ConfigView> {
        let v = view(doc, kind, revision, exists)?;
        if endpoint(doc, kind)?.is_none() && kind != EndpointKind::Openai {
            return Err(err("INVALID_CONFIG"));
        }
        Ok(v)
    }
    pub fn with_config<T>(&self, f: impl FnOnce(&ConfigSnapshot) -> T) -> Result<T> {
        self.with_endpoint(EndpointKind::Openai, f)
    }
    pub fn with_endpoint<T>(
        &self,
        kind: EndpointKind,
        f: impl FnOnce(&ConfigSnapshot) -> T,
    ) -> Result<T> {
        let _lock = self.lock()?;
        let (doc, rev, exists) = self.document()?;
        let v = self.required_view(&doc, kind, rev, exists)?;
        let s = self.decrypt(field_in(&doc, kind, "api_key")?)?;
        validate(&v.base_url, &v.model, &s.0, kind.requires_key())?;
        let snapshot = ConfigSnapshot {
            endpoint: kind,
            base_url: v.base_url,
            model: v.model,
            encrypted: v.encrypted,
            secret: s,
        };
        drop(_lock);
        Ok(f(&snapshot))
    }
    pub fn with_secret<T>(&self, f: impl FnOnce(&str) -> T) -> Result<T> {
        self.with_config(|c| c.with_secret(f))
    }
    /// Portable YAML for encrypted off-machine sync. The returned buffer holds
    /// the plaintext credential and is zeroized on drop; callers must encrypt it
    /// before writing it anywhere persistent.
    pub fn export_portable(&self) -> Result<Zeroizing<String>> {
        self.sync_export()?
            .map(|(text, _, _)| text)
            .ok_or_else(|| err("INVALID_CONFIG"))
    }
    pub(crate) fn sync_export(&self) -> Result<Option<(Zeroizing<String>, String, String)>> {
        let _lock = self.lock()?;
        let (doc, revision, exists) = self.document()?;
        if !exists {
            return Ok(None);
        }
        let (mut plain, _) = self.plain_document(&doc)?;
        validate_document(&plain)?;
        let text =
            Zeroizing::new(serde_yaml::to_string(&plain).map_err(|_| err("INVALID_CONFIG"))?);
        wipe_yaml(&mut plain);
        let modified: chrono::DateTime<chrono::Utc> = fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .map_err(|_| err("IO"))?
            .into();
        Ok(Some((text, revision, modified.to_rfc3339())))
    }
    pub(crate) fn sync_import(
        &self,
        content: &[u8],
        expected_revision: &str,
        updated_at: &str,
    ) -> Result<()> {
        let doc = parse(std::str::from_utf8(content).map_err(|_| err("SYNC_BUNDLE"))?)?;
        if EndpointKind::ALL
            .iter()
            .any(|kind| field_in(&doc, *kind, "api_key").is_ok_and(|key| key.starts_with("ENC(")))
        {
            return Err(err("SYNC_BUNDLE"));
        }
        validate_document(&doc)?;
        let current = self.load()?;
        let encrypt = !current.exists || current.encrypted;
        let (old_doc, _, old_exists) = self.document()?;
        let old_plain = if old_exists {
            decrypt_document(self, &old_doc).unwrap_or(old_doc)
        } else {
            Value::Mapping(Mapping::new())
        };
        let diff = self.import_diff(&old_plain, &doc)?;
        let modified: std::time::SystemTime = chrono::DateTime::parse_from_rfc3339(updated_at)
            .map_err(|_| err("SYNC_BUNDLE"))?
            .into();
        self.commit_document(
            doc,
            encrypt,
            "skill",
            Some(modified),
            expected_revision,
            EndpointKind::Openai,
            diff,
            None,
        )
        .map(|_| ())
        .map_err(|error| {
            if error.code == "CONFLICT" {
                err("SYNC_LOCAL_CHANGED")
            } else {
                error
            }
        })
    }
    pub fn prune_history_before(&self, cutoff: chrono::DateTime<chrono::Utc>) -> Result<usize> {
        let _lock = self.lock()?;
        self.recover()?;
        let mut entries = self.read_history()?;
        let before = entries.len();
        entries.retain(|entry| {
            chrono::DateTime::parse_from_rfc3339(&entry.timestamp).is_ok_and(|time| time >= cutoff)
        });
        if entries.len() != before {
            write_atomic(
                &self.side(".history.json"),
                &serde_json::to_vec_pretty(&entries).map_err(|_| err("IO"))?,
            )?;
        }
        Ok(before - entries.len())
    }
    pub fn validate_secret(&self) -> Result<()> {
        self.with_secret(|_| ())
    }
    pub fn validate_endpoint(&self, kind: EndpointKind) -> Result<()> {
        self.with_endpoint(kind, |_| ())
    }
    /// Explicit user-triggered `GET {base_url}/models` request. The lock is
    /// released before the network call, and the response body never leaves
    /// this function.
    pub fn test_endpoint(&self) -> Result<ProbeResult> {
        self.test_endpoint_kind(EndpointKind::Openai)
    }
    pub fn test_endpoint_kind(&self, kind: EndpointKind) -> Result<ProbeResult> {
        self.with_endpoint(kind, |config| {
            let base_url = config.base_url.clone();
            config.with_secret(|api_key| probe::probe(&base_url, api_key))
        })
    }
    pub fn history(&self) -> Result<Vec<HistoryEntry>> {
        let _lock = self.lock()?;
        self.recover()?;
        self.read_history()
    }
    fn read_history(&self) -> Result<Vec<HistoryEntry>> {
        self.read_history_snapshot().map(|(history, _)| history)
    }
    fn read_history_snapshot(&self) -> Result<(Vec<HistoryEntry>, String)> {
        let (history, revision) = match read_bounded(&self.side(".history.json"))? {
            None => (vec![], "missing".into()),
            Some(raw) => (
                serde_json::from_slice(&raw).map_err(|_| err("HISTORY_CORRUPT"))?,
                hash(&raw),
            ),
        };
        validate_history(&history)?;
        Ok((history, revision))
    }
    fn history_revision(&self) -> Result<String> {
        Ok(read_bounded(&self.side(".history.json"))?
            .map(|bytes| hash(&bytes))
            .unwrap_or_else(|| "missing".into()))
    }
    fn recover(&self) -> Result<()> {
        let p = self.side(".pending.json");
        if let Some(raw) = read_bounded(&p)? {
            let j: Journal = serde_json::from_slice(&raw).map_err(|_| err("HISTORY_CORRUPT"))?;
            validate_history(&j.history)?;
            let valid_hash =
                |value: &str| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit());
            if !valid_hash(&j.after_revision)
                || !(j.before_revision == "missing" || valid_hash(&j.before_revision))
                || !(j.before_history_revision == "missing"
                    || valid_hash(&j.before_history_revision))
                || j.history.is_empty()
            {
                return Err(err("HISTORY_CORRUPT"));
            }
            let (_, rev, _) = self.document()?;
            if j.before_revision == j.after_revision {
                return Err(err("HISTORY_CORRUPT"));
            }
            let intended =
                serde_json::to_vec_pretty(&j.history).map_err(|_| err("HISTORY_CORRUPT"))?;
            let current_history_revision = self.history_revision()?;
            if rev == j.after_revision {
                if current_history_revision == hash(&intended) {
                    // Already committed: only remove the journal.
                } else if current_history_revision == j.before_history_revision {
                    write_atomic(&self.side(".history.json"), &intended)?;
                } else {
                    return Err(err("HISTORY_CORRUPT"));
                }
            } else {
                // The configuration was not replaced by this transaction, so the journal
                // is a stale intent: the save failed before replacement, and the file may
                // since have been rewritten by another writer. History bytes are only ever
                // written after a confirmed replacement, so they cannot belong to this
                // journal and must not be touched here. Dropping the stale journal is the
                // only action that cannot destroy committed evidence and cannot deadlock
                // the next save with a misleading HISTORY_CORRUPT.
                debug_assert_ne!(rev, j.after_revision);
            }
            fs::remove_file(p).map_err(|_| err("IO"))?;
            sync_parent(&self.path)?;
        }
        Ok(())
    }
    pub fn save(&self, request: SaveRequest) -> Result<ConfigView> {
        self.save_with_source(request, "desktop")
    }
    pub fn save_with_source(&self, request: SaveRequest, source: &str) -> Result<ConfigView> {
        self.save_endpoint(EndpointKind::Openai, request, source)
    }
    pub fn save_endpoint(
        &self,
        kind: EndpointKind,
        request: SaveRequest,
        source: &str,
    ) -> Result<ConfigView> {
        if !matches!(source, "desktop" | "cli" | "skill" | "test") {
            return Err(err("INVALID_SOURCE"));
        }
        // Reject an already-stale revision before validating the submitted
        // values, matching the historical precedence of CONFLICT. The commit
        // repeats this check atomically after the merge has been prepared.
        {
            let _lock = self.lock()?;
            self.recover()?;
            let (_, revision, _) = self.document()?;
            if revision != request.expected_revision {
                return Err(err("CONFLICT"));
            }
        }
        // Read without locking to build the merged plaintext document. The commit
        // below re-reads under the lock and rejects a stale revision, so a
        // concurrent writer turns this into CONFLICT instead of a lost update.
        let (doc, revision, exists) = self.document()?;
        let mut plain = if exists {
            doc.clone()
        } else {
            Value::Mapping(Mapping::new())
        };
        let old = view(&doc, kind, revision.clone(), exists)?;
        let old_key = field_in(&doc, kind, "api_key")?;
        let secret = match request.api_key.as_ref() {
            Some(key) => Secret(key.clone()),
            None if old.configured => self.decrypt(old_key)?,
            None => Secret(String::new()),
        };
        let mut request = request;
        let key_changed = request.api_key.is_some();
        validate(
            &request.base_url,
            &request.model,
            &secret.0,
            kind.requires_key(),
        )?;
        set_fields(
            &mut plain,
            kind,
            &request.base_url,
            &request.model,
            &secret.0,
        )?;
        if let Some(key) = request.api_key.as_mut() {
            key.zeroize();
        }
        let diff = self.document_diff(
            &doc,
            &plain,
            &old,
            old_key,
            key_changed,
            kind,
            request.encrypt,
        )?;
        let view = self.commit_document(
            plain,
            request.encrypt,
            source,
            None,
            &request.expected_revision,
            kind,
            diff,
            Some(kind),
        )?;
        // A committed save is the only trigger a scheduler needs; the flag is
        // debounced by the scheduler's next sweep rather than spawning work here.
        let _ = self.mark_dirty();
        Ok(view)
    }
    /// One redacted diff for the endpoint being edited, or a full diff when
    /// importing a portable document (`kind` is its primary endpoint).
    #[allow(clippy::too_many_arguments)]
    fn document_diff(
        &self,
        old_doc: &Value,
        new_plain: &Value,
        old: &ConfigView,
        old_key: &str,
        key_changed: bool,
        kind: EndpointKind,
        encrypt: bool,
    ) -> Result<Vec<FieldDiff>> {
        let mut diff = Vec::new();
        for (name, before, after) in [
            (
                "base_url",
                old.base_url.clone(),
                field_in(new_plain, kind, "base_url")?.to_string(),
            ),
            (
                "model",
                old.model.clone(),
                field_in(new_plain, kind, "model")?.to_string(),
            ),
        ] {
            if before != after {
                diff.push(FieldDiff {
                    field: field_name(kind, name),
                    before: truncate(before),
                    after: truncate(after),
                });
            }
        }
        let before_encrypted = old_key.starts_with("ENC(");
        if old.configured && before_encrypted != encrypt {
            diff.push(FieldDiff {
                field: field_name(kind, "encrypted"),
                before: before_encrypted.to_string(),
                after: encrypt.to_string(),
            });
        }
        let new_key = field_in(new_plain, kind, "api_key")?;
        if key_changed || old.key_present == new_key.is_empty() {
            diff.push(FieldDiff {
                field: field_name(kind, "api_key"),
                before: if old.key_present {
                    "[REDACTED]"
                } else {
                    "[EMPTY]"
                }
                .into(),
                after: if new_key.is_empty() {
                    "[EMPTY]"
                } else {
                    "[REDACTED]"
                }
                .into(),
            });
        }
        // An imported document can also change the other blocks; report their
        // address/model changes without exposing their credentials.
        if kind == EndpointKind::Openai {
            for other in EndpointKind::ALL
                .iter()
                .copied()
                .filter(|other| *other != kind)
            {
                if endpoint(old_doc, other)?.is_none() && endpoint(new_plain, other)?.is_none() {
                    continue;
                }
                for name in ["base_url", "model"] {
                    let before = field_in(old_doc, other, name)?;
                    let after = field_in(new_plain, other, name)?;
                    if before != after {
                        diff.push(FieldDiff {
                            field: field_name(other, name),
                            before: truncate(before.into()),
                            after: truncate(after.into()),
                        });
                    }
                }
                if endpoint(old_doc, other)?.is_some() != endpoint(new_plain, other)?.is_some() {
                    diff.push(FieldDiff {
                        field: field_name(other, "configured"),
                        before: endpoint(old_doc, other)?.is_some().to_string(),
                        after: endpoint(new_plain, other)?.is_some().to_string(),
                    });
                }
                let before_key = !field_in(old_doc, other, "api_key")?.is_empty();
                let after_key = !field_in(new_plain, other, "api_key")?.is_empty();
                if before_key != after_key {
                    diff.push(FieldDiff {
                        field: field_name(other, "api_key"),
                        before: if before_key { "[REDACTED]" } else { "[EMPTY]" }.into(),
                        after: if after_key { "[REDACTED]" } else { "[EMPTY]" }.into(),
                    });
                }
            }
        }
        if diff.len() > 20 {
            return Err(err("TOO_LARGE"));
        }
        Ok(diff)
    }
    #[allow(clippy::too_many_arguments)]
    fn commit_document(
        &self,
        mut plain: Value,
        encrypt: bool,
        source: &str,
        modified: Option<std::time::SystemTime>,
        expected_revision: &str,
        kind: EndpointKind,
        diff: Vec<FieldDiff>,
        only: Option<EndpointKind>,
    ) -> Result<ConfigView> {
        let _lock = self.lock()?;
        self.recover()?;
        let (_, revision, _) = self.document()?;
        if revision != expected_revision {
            return Err(err("CONFLICT"));
        }
        let (mut history, before_history_revision) = self.read_history_snapshot()?;
        // Best effort: normalize other encrypted endpoints for a stable semantic
        // hash. A temporarily unavailable key must not block saving this endpoint.
        let mut canonical_source = decrypt_document(self, &plain).unwrap_or_else(|_| plain.clone());
        let canonical = {
            let mut tree = canonical_json(&canonical_source)?;
            let bytes =
                Zeroizing::new(serde_json::to_vec(&tree).map_err(|_| err("INVALID_CONFIG"))?);
            let hash = hash(&bytes);
            wipe_json(&mut tree);
            hash
        };
        wipe_yaml(&mut canonical_source);
        let stored = store_document(self, &plain, encrypt, only)?;
        wipe_yaml(&mut plain);
        let mut id_bytes = [0u8; 16];
        OsRng.fill_bytes(&mut id_bytes);
        let id: String = id_bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut serialized = Zeroizing::new(format!("# ai-config-manager transaction: {id}\n"));
        serialized.push_str(&serde_yaml::to_string(&stored).map_err(|_| err("INVALID_CONFIG"))?);
        if serialized.len() as u64 > MAX_FILE {
            return Err(err("TOO_LARGE"));
        }
        let after_revision = hash(serialized.as_bytes());
        history.insert(
            0,
            HistoryEntry {
                id,
                timestamp: chrono::Utc::now().to_rfc3339(),
                diff,
                sha256: canonical,
                source: source.into(),
            },
        );
        history.truncate(10);
        validate_history(&history)?;
        let journal = Journal {
            before_revision: revision.clone(),
            after_revision: after_revision.clone(),
            before_history_revision: before_history_revision.clone(),
            history,
        };
        let journal_bytes = serde_json::to_vec_pretty(&journal).map_err(|_| err("IO"))?;
        write_atomic(&self.side(".pending.json"), &journal_bytes)?;
        let (_, current, _) = self.document()?;
        if current != revision || self.history_revision()? != before_history_revision {
            fs::remove_file(self.side(".pending.json")).map_err(|_| err("IO"))?;
            return Err(err("CONFLICT"));
        }
        if let Err(error) = write_atomic(&self.path, serialized.as_bytes()) {
            return match self.document() {
                Ok((_, current, _)) if current == after_revision => Err(err("HISTORY_PENDING")),
                Ok((_, current, _)) if current == revision => Err(error),
                _ => Err(err("COMMIT_UNCERTAIN")),
            };
        }
        // Once replacement succeeds the durable journal is authoritative. Recovery is idempotent.
        if self.recover().is_err() {
            return Err(err("HISTORY_PENDING"));
        }
        if let Some(modified) = modified {
            set_modified(&self.path, modified).map_err(|_| err("COMMIT_UNCERTAIN"))?;
        }
        view(&stored, kind, after_revision, true)
    }
    fn plain_document(&self, doc: &Value) -> Result<(Value, String)> {
        let plain = decrypt_document(self, doc)?;
        Ok((
            plain,
            field_in(doc, EndpointKind::Openai, "api_key")?.to_string(),
        ))
    }
    /// Diff for a portable document import. Both sides are plaintext, so
    /// credentials can be compared without writing them into history.
    fn import_diff(&self, old: &Value, new: &Value) -> Result<Vec<FieldDiff>> {
        let mut diff = Vec::new();
        for kind in EndpointKind::ALL {
            let old_present = endpoint(old, kind)?.is_some();
            let new_present = endpoint(new, kind)?.is_some();
            if !old_present && !new_present {
                continue;
            }
            for name in ["base_url", "model"] {
                let before = field_in(old, kind, name)?;
                let after = field_in(new, kind, name)?;
                if before != after {
                    diff.push(FieldDiff {
                        field: field_name(kind, name),
                        before: truncate(before.into()),
                        after: truncate(after.into()),
                    });
                }
            }
            if old_present != new_present {
                diff.push(FieldDiff {
                    field: field_name(kind, "configured"),
                    before: old_present.to_string(),
                    after: new_present.to_string(),
                });
            }
            let before_key = field_in(old, kind, "api_key")?;
            let after_key = field_in(new, kind, "api_key")?;
            if before_key != after_key {
                diff.push(FieldDiff {
                    field: field_name(kind, "api_key"),
                    before: if before_key.is_empty() {
                        "[EMPTY]"
                    } else {
                        "[REDACTED]"
                    }
                    .into(),
                    after: if after_key.is_empty() {
                        "[EMPTY]"
                    } else {
                        "[REDACTED]"
                    }
                    .into(),
                });
            }
        }
        if diff.len() > 20 {
            return Err(err("TOO_LARGE"));
        }
        Ok(diff)
    }
    fn mark_dirty(&self) -> Result<()> {
        write_atomic(&self.side(".dirty"), b"1")
    }
    /// Debounce marker written by a successful save. The scheduler promotes it
    /// to an early sync and only clears it once the request has been accepted.
    pub fn has_dirty_flag(&self) -> bool {
        self.side(".dirty").exists()
    }
    pub fn clear_dirty_flag(&self) {
        let _ = fs::remove_file(self.side(".dirty"));
    }
}

fn field_name(kind: EndpointKind, name: &str) -> String {
    if kind == EndpointKind::Openai {
        name.into()
    } else {
        format!("{}.{name}", kind.key())
    }
}
fn truncate(value: String) -> String {
    if value.len() > 8192 {
        "[VALUE EXCEEDS LIMIT]".into()
    } else {
        value
    }
}

#[cfg(test)]
mod commit_tests {
    use super::*;
    struct UnusedVault;
    impl Vault for UnusedVault {
        fn get_key(&self) -> Result<Option<SecretKey>> {
            panic!("plaintext test must not use vault")
        }
        fn create_key(&self) -> Result<SecretKey> {
            panic!("plaintext test must not use vault")
        }
    }
    struct FixedVault;
    impl Vault for FixedVault {
        fn get_key(&self) -> Result<Option<SecretKey>> {
            Ok(Some(SecretKey::new([7; 32])))
        }
        fn create_key(&self) -> Result<SecretKey> {
            Ok(SecretKey::new([7; 32]))
        }
    }
    #[test]
    fn named_endpoints_are_independent_encrypted_and_portable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().canonicalize().unwrap().join("ai.yml");
        let store = Store::new(path.clone(), Arc::new(FixedVault));
        let openai = store
            .save(SaveRequest {
                base_url: "https://example.com/v1".into(),
                model: "chat-model".into(),
                api_key: Some("openai-secret".into()),
                encrypt: true,
                expected_revision: "missing".into(),
            })
            .unwrap();
        let missing = store.load_endpoint(EndpointKind::Embedding).unwrap();
        assert!(!missing.configured);
        assert_eq!(
            store
                .with_endpoint(EndpointKind::Embedding, |_| ())
                .unwrap_err()
                .code,
            "INVALID_CONFIG"
        );
        let embedding = store
            .save_endpoint(
                EndpointKind::Embedding,
                SaveRequest {
                    base_url: "http://127.0.0.1:8848/v1".into(),
                    model: "embedding-model".into(),
                    api_key: None,
                    encrypt: true,
                    expected_revision: openai.revision.clone(),
                },
                "desktop",
            )
            .unwrap();
        assert!(embedding.configured && !embedding.key_present);
        let laya = store
            .save_endpoint(
                EndpointKind::Laya,
                SaveRequest {
                    base_url: "http://127.0.0.1:8848/v1".into(),
                    model: "laya-multilingual".into(),
                    api_key: None,
                    encrypt: true,
                    expected_revision: embedding.revision.clone(),
                },
                "desktop",
            )
            .unwrap();
        assert_eq!(
            store
                .save_endpoint(
                    EndpointKind::Cosyvoice,
                    SaveRequest {
                        base_url: "http://127.0.0.1:8848/v1".into(),
                        model: "cosyvoice3-0.5b-2512".into(),
                        api_key: Some("voice-secret".into()),
                        encrypt: true,
                        expected_revision: openai.revision.clone(),
                    },
                    "desktop",
                )
                .unwrap_err()
                .code,
            "CONFLICT"
        );
        let voice = store
            .save_endpoint(
                EndpointKind::Cosyvoice,
                SaveRequest {
                    base_url: "http://127.0.0.1:8848/v1".into(),
                    model: "cosyvoice3-0.5b-2512".into(),
                    api_key: Some("voice-secret".into()),
                    encrypt: true,
                    expected_revision: laya.revision,
                },
                "desktop",
            )
            .unwrap();
        assert!(voice.configured && voice.key_present && voice.encrypted);
        assert_eq!(store.with_secret(str::to_string).unwrap(), "openai-secret");
        let history = store.history().unwrap();
        assert!(history
            .iter()
            .any(|entry| entry.diff.iter().any(|diff| diff.field == "laya.model")));
        let portable = store.export_portable().unwrap();
        assert!(portable.contains("openai-secret"));
        assert!(portable.contains("voice-secret"));
        assert!(portable.contains("laya-multilingual"));
        assert!(!portable.contains("ENC("));

        let target_directory = tempfile::tempdir().unwrap();
        let target_path = target_directory
            .path()
            .canonicalize()
            .unwrap()
            .join("ai.yml");
        let target = Store::new(target_path.clone(), Arc::new(FixedVault));
        target
            .sync_import(portable.as_bytes(), "missing", "2026-01-02T00:00:00Z")
            .unwrap();
        assert_eq!(
            target.load_endpoint(EndpointKind::Laya).unwrap().model,
            "laya-multilingual"
        );
        assert!(
            target
                .load_endpoint(EndpointKind::Cosyvoice)
                .unwrap()
                .key_present
        );
        assert_eq!(target.with_secret(str::to_string).unwrap(), "openai-secret");
        assert_eq!(
            target
                .with_endpoint(EndpointKind::Cosyvoice, |config| {
                    config.with_secret(str::to_string)
                })
                .unwrap(),
            "voice-secret"
        );
        let disk = fs::read_to_string(target_path).unwrap();
        for secret in ["openai-secret", "voice-secret"] {
            assert!(!disk.contains(secret));
        }
    }
    #[test]
    fn save_request_defaults_to_encrypted_storage() {
        let request: SaveRequest = serde_json::from_str(
            r#"{"base_url":"https://example.com/v1","model":"test","expected_revision":"missing"}"#,
        )
        .unwrap();
        assert!(request.encrypt);
    }
    #[test]
    fn portable_sync_import_preserves_encryption_unknown_values_and_revision_checks() {
        struct MemoryVault;
        impl Vault for MemoryVault {
            fn get_key(&self) -> Result<Option<SecretKey>> {
                Ok(Some(SecretKey::new([7; 32])))
            }
            fn create_key(&self) -> Result<SecretKey> {
                Ok(SecretKey::new([7; 32]))
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().canonicalize().unwrap().join("ai.yml");
        let store = Store::new(path.clone(), Arc::new(MemoryVault));
        let original = store
            .save(SaveRequest {
                base_url: "https://example.com/v1".into(),
                model: "old".into(),
                api_key: Some("old-secret".into()),
                encrypt: true,
                expected_revision: "missing".into(),
            })
            .unwrap();
        let portable = b"ai:\n  openai:\n    base_url: https://remote.example/v1\n    model: newer\n    api_key: portable-new-secret\ncustom:\n  enabled: true\n";
        store
            .sync_import(portable, &original.revision, "2026-01-02T00:00:00Z")
            .unwrap();
        let view = store.load().unwrap();
        assert!(view.encrypted);
        assert_eq!(view.model, "newer");
        assert_eq!(
            store.with_secret(str::to_string).unwrap(),
            "portable-new-secret"
        );
        let bytes = fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("portable-new-secret"));
        assert!(store.export_portable().unwrap().contains("custom:"));
        assert_eq!(store.history().unwrap().len(), 2);
        assert_eq!(
            store
                .sync_import(portable, &original.revision, "2026-01-03T00:00:00Z")
                .unwrap_err()
                .code,
            "SYNC_LOCAL_CHANGED"
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    #[test]
    fn history_pruning_recovers_a_committed_pending_save_first() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().canonicalize().unwrap().join("ai.yml");
        let store = Store::new(path, Arc::new(UnusedVault));
        let request = SaveRequest {
            base_url: "https://example.com/v1".into(),
            model: "model".into(),
            api_key: Some("synthetic-key".into()),
            encrypt: false,
            expected_revision: "missing".into(),
        };
        filesystem::FAIL_WRITE
            .with(|fault| *fault.borrow_mut() = Some((store.side(".history.json"), false)));
        assert_eq!(store.save(request).unwrap_err().code, "HISTORY_PENDING");
        assert!(store.side(".pending.json").exists());
        let revision = store.load().unwrap().revision;
        assert_eq!(
            store
                .prune_history_before(chrono::Utc::now() + chrono::Duration::seconds(1))
                .unwrap(),
            1
        );
        assert!(!store.side(".pending.json").exists());
        assert!(store.history().unwrap().is_empty());
        assert_eq!(store.load().unwrap().revision, revision);
    }

    #[test]
    fn config_write_failures_preserve_history_and_report_commit_boundary() {
        for after_replace in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().canonicalize().unwrap().join("ai.yml");
            let store = Store::new(path.clone(), Arc::new(UnusedVault));
            let request = |revision: String| SaveRequest {
                base_url: "https://example.com/v1".into(),
                model: "test".into(),
                api_key: Some("secret".into()),
                encrypt: false,
                expected_revision: revision,
            };
            let first = store.save(request("missing".into())).unwrap();
            let history_before = fs::read(store.side(".history.json")).unwrap();
            filesystem::FAIL_WRITE.with(|f| *f.borrow_mut() = Some((path.clone(), after_replace)));
            let error = store.save(request(first.revision.clone())).unwrap_err();
            assert_eq!(
                error.code,
                if after_replace {
                    "HISTORY_PENDING"
                } else {
                    "IO"
                }
            );
            assert_eq!(
                fs::read(store.side(".history.json")).unwrap(),
                history_before
            );
            assert_eq!(
                store.load().unwrap().revision == first.revision,
                !after_replace
            );
            assert_eq!(
                store.history().unwrap().len(),
                if after_replace { 2 } else { 1 }
            );
        }
    }
}
