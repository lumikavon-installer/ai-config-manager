# First-party application integration

Applications own their portable preferences. AI Config Manager owns registration,
encrypted remote bundles, scheduling, and synchronization records. A manager run
and an application's Sync now action use the same registered callback and merge
algorithm. No database, live config file, or remote-provided path is copied over
an application's storage.

## Shared Rust bridge contract

`integrations/rust` is a standalone Rust crate named `ai-config-sync`, imported
by sibling applications using a relative Cargo path. It has no Tauri dependency.
Its public API is:

```rust
pub type Result<T> = std::result::Result<T, String>;
pub trait ConfigAdapter: Send + Sync + 'static {
    fn read(&self) -> Result<serde_json::Value>;
    fn apply(&self, value: serde_json::Value, expected_sha256: &str) -> Result<()>;
}
pub struct SyncOptions {
    pub app_id: String,
    pub app_name: String,
    pub config_id: String,
    pub label: String,
    pub state_dir: std::path::PathBuf,
}
pub struct SyncHandle { /* Clone; retain for the process lifetime */ }
pub fn start(options: SyncOptions, adapter: std::sync::Arc<dyn ConfigAdapter>) -> Result<SyncHandle>;
pub fn digest(value: &serde_json::Value) -> Result<String>;
impl SyncHandle {
    pub fn status(&self) -> Result<serde_json::Value>;
    pub fn sync_now(&self) -> Result<serde_json::Value>;
    pub fn set_enabled(&self, enabled: bool) -> Result<serde_json::Value>;
    pub fn set_auto_sync(&self, enabled: bool) -> Result<serde_json::Value>;
    pub fn local_changed(&self) -> Result<()>;
}
```

Errors are fixed codes (including `SYNC_LOCAL_CHANGED`, `SYNC_CONFIG`,
`AGENT_UNAVAILABLE`, `SYNC_BUSY`); never configuration bodies or OS errors.
`digest` hashes compact canonical JSON with recursively sorted object keys.
Adapters export an explicit allowlist, validate incoming types and values, merge
into the current settings, and compare `digest(read())` with `expected_sha256`
under the same mutation lock as ordinary saves. An adapter commits through the
application's existing settings transaction and refreshes native/runtime/UI state.
The callback is private AF_UNIX HTTP GET/PUT `/config`, with the existing manager
base64/timestamp/digest envelope and 1 MiB decoded limit.

The bridge persists digest/timestamp metadata, never a second configuration copy.
Initial unversioned defaults use an epoch timestamp so they cannot supersede an
existing remote configuration. Call `local_changed` after a successful user save;
the bridge also observes changes periodically. Incoming timestamps are retained,
including after restart, to prevent echo uploads. A lost write reply is never
retried as an unguarded local mutation.

Startup registration retries while the manager is unavailable. Re-registration
preserves manager-owned enabled/automatic choices and does not reset deadlines.
Socket serving is independent of blocking manager requests, preventing callback
deadlock during Sync now. Stopping the last handle stops background work and
removes only its own socket. Missing manager is a visible, nonfatal state.

Manager JSON operations added for this integration:

- `sync_run_application { app_id }`: manually sync enabled items belonging to this
  application, retaining unrelated remote entries.
- `sync_set_application_auto_sync { app_id, enabled }`: set automatic participation
  for the application's registered items; manual synchronization stays available.

`SyncItem.auto_sync` defaults to true for existing registrations. Global automatic
policy still controls the scheduler. Scheduled runs select `enabled && auto_sync`;
manual runs select enabled items. Applications display their registration state,
automatic participation, manager availability, last result, and a manual button.
Manager-side controls expose the same per-application operations.

## Application boundaries

The actual directory names are `quietkey` and `vs-workspace`;
`project-io-manager` and `prompt-workbench` are separate applications. Existing
VS Workspace callback behavior is retained, with its missing controls added.
The other adapters synchronize portable preferences, not business records,
credentials, local paths, OS launch registration, or permission grants.
ConfigCenter synchronizes its theme and close-to-tray preference through the
same SQLite write queue as ordinary saves; autostart remains a local OS
registration and is never imported.

Web services must run under the manager's local OS account to reach its socket.
Browser code never receives the token or local filesystem paths. Multi-user
services require an explicit administrator-owned application preferences scope;
sync-control endpoints retain existing authentication, authorization and CSRF
checks. Web defaults must be consumed by actual application UI, with active
drafts preserved when defaults change.

Verification uses temporary app settings, synthetic configurations and a local
test Git repository. It must cover callback application, stale digest rejection,
registration retry, timestamp stability, scoped manual sync, automatic selection,
and retained local-only fields. Native vault, installer and full GUI acceptance
are reported separately from unit/build checks.
