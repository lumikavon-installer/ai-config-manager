# ai-config-sync

Shared Rust bridge for first-party applications. This is a standalone crate with
its own workspace and lockfile, no Tauri dependency, and the public API frozen in
[`docs/application-integration.md`](../../docs/application-integration.md).

```toml
[dependencies]
ai-config-sync = { path = "../ai-config-manager/integrations/rust" }
```

Retain the handle for the application process lifetime:

```rust,ignore
let sync = ai_config_sync::start(
    ai_config_sync::SyncOptions {
        app_id: "my-app".into(),
        app_name: "My App".into(),
        config_id: "preferences".into(),
        label: "Portable preferences".into(),
        state_dir: application_config_directory,
    },
    std::sync::Arc::new(MySettingsAdapter::new(settings)),
)?;
```

`state_dir` must be absolute. The bridge creates its own private child directory;
it does not change permissions on the application directory. Each app/config
pair permits one active instance. Additional starts return `SYNC_BUSY`. On
Windows it uses `uds_windows` AF_UNIX sockets and a protected current-user DACL;
Unix uses directories with mode `0700` and sockets/files with mode `0600`. Long
socket paths fall back to a private directory under the canonical system temp
directory, and paths exceeding 100 bytes are rejected.

Implement `ConfigAdapter::read` as a JSON allowlist of portable preferences.
`apply(value, expected_sha256)` must validate types/values, acquire the normal
settings mutation lock, compare `ai_config_sync::digest(&current_export)` with
`expected_sha256`, and commit through the application's existing transaction.
Return `SYNC_LOCAL_CHANGED` without mutation when the digest differs. Preserve
local-only settings and refresh runtime/UI state after committing. Release the
application lock before calling `handle.local_changed()` after a user save.
Adapter errors containing arbitrary text are replaced with fixed codes.

The digest hashes compact JSON with recursively sorted object keys, including
when a consumer enables serde_json's `preserve_order` feature. Array order and
JSON scalar types remain significant. Exported and imported content is limited
to 1 MiB. The callback serves private HTTP `GET /config` and `PUT /config`, using
the manager's base64/timestamp/expected-digest envelope. Callback authorization
comes from filesystem permissions; the manager token is sent only to the
manager's newline JSON socket protocol. Callback responses use
`Cache-Control: no-store`.

The bridge stores only digest/timestamp metadata. First unversioned settings use
the Unix epoch. Actual local changes receive a strictly newer timestamp; an
unchanged `local_changed()` call does not create a version. Incoming timestamps
are retained exactly. Before an incoming apply, the bridge atomically persists
a pending digest/timestamp. A restart resolves this journal from the adapter's
current export. A post-commit metadata failure therefore retains the remote
timestamp, including when the write reply was lost. Ambiguous adapter failures
retain the incoming timestamp conservatively; explicit CAS conflicts clear the
pending import. No application configuration body is persisted by the bridge.

Startup succeeds while the manager is absent. A worker observes local changes
and checks registration every 30 seconds. Registration preserves the existing
enabled flag; the manager preserves automatic participation. Matching
registrations are not rewritten. This worker does not initiate automatic sync
runs: the manager owns scheduling. Callback serving runs on an independent
thread so a manager request can call back while `sync_now()` waits.

`status()`, `sync_now()`, `set_enabled(bool)`, and `set_auto_sync(bool)` return
the same sanitized JSON shape:

```json
{
  "registered": true,
  "enabled": true,
  "auto_sync": true,
  "manager_auto_sync": true,
  "last_synced_at": "2026-01-12T08:09:10Z",
  "error_code": null,
  "running": false
}
```

Registration, enabled state, and automatic participation describe this
app/config pair. `set_auto_sync` changes participation for all registered configs
of the application; `set_enabled` changes this item. `sync_now` sends
`sync_run_application { app_id }`. `set_auto_sync` sends
`sync_set_application_auto_sync { app_id, enabled }`. Global policy still gates
automatic scheduling. `running` is the manager's global busy state.
`last_synced_at` and the latest result use records for this application or an
applicable manager-wide run; another application's manual result is ignored.
Scheduled records are considered only while this item is enabled for automatic
sync. Run completion does not prove every item succeeded: per-item deferred
errors are filtered to this registration and take precedence. Older managers
without records fall back to global status. Fixed manager error codes, including
`SYNC_DISABLED` and `SYNC_NOT_CONFIGURED`, are preserved; unknown errors become
`SYNC_FAILED`. No full manager view, repository location, callback path,
configuration, token, arbitrary error message, or OS error reaches these results.

`status()` reports manager failures in `error_code` and retains the last known
choices. Before a first successful manager read these choices are false.
Explicit control methods return fixed error codes on failure. A failed sync
reply can represent an uncertain completed operation and is not retried
automatically. Manager requests have a 1-second connect bound, 20-second normal
I/O bounds, and a 600-second manual-run response bound. Callback framing has
16 KiB headers, 2 MiB encoded bodies, and a 10-second I/O deadline.

Dropping the last cloned handle cancels active I/O and wakes both workers without
joining a long manager request. Workers remove only their own socket identity
and release the instance lock as they exit. Application adapter code already
executing cannot be forcibly interrupted; adapters should complete promptly.
A replacement start may briefly return `SYNC_BUSY` during shutdown. Socket names
are unique per start, so existing sockets are never unlinked to make room. An
unclean process exit can leave an unused socket entry in the private directory.

Verification requires Git and uses synthetic settings and temporary directories:

```console
cargo test --locked --manifest-path integrations/rust/Cargo.toml
cargo fmt --manifest-path integrations/rust/Cargo.toml -- --check
cargo clippy --locked --manifest-path integrations/rust/Cargo.toml --all-targets -- -D warnings
```

Tests cover callback GET/PUT, recursively canonical bytes, epoch initialization,
adapter CAS races, restart and metadata-failure recovery, background registration
retry, preserved manager choices, token rejection, bounded framing, socket ACLs,
short-path fallback, active-socket preservation, and shutdown during a stalled
manager request. End-to-end tests also run the production `ai-config-core`
`SyncStore`, authenticated agent dispatch, shared bridge callbacks, encrypted
bundles and a temporary local bare Git repository. Two isolated managers verify
manager/application manual actions, automatic selection and global policy,
remote imports, retained local-only fields, no echo commits after restart,
subsequent local uploads, callback CAS conflicts and application-scoped status.
Scheduled tests reload an isolated persisted deadline instead of waiting for a
wall-clock interval. All manager paths are injected privately under `cfg(test)`;
credentials use an in-memory test vault. No real user configuration or native
credential vault is accessed. This does not verify desktop UI, native vault,
remote-host credentials or installers.
