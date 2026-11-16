# Application registration, scheduling and sync records

## Repository backends and permission test

Sync settings and `SyncView` add `provider: "github" | "git"` (default `github`) and `git_branch` (default `main`). Existing GitHub settings and clients remain compatible. GitHub `repo` remains `owner/repository`; custom Git accepts HTTPS, SSH, SCP-style SSH or an absolute local repository path. Custom Git uses the installed Git client with the current user's credential helper / SSH agent, with interactive prompts disabled. Repository URLs must not contain embedded passwords, HTTP usernames, query strings or fragments. Only the encrypted `.ai-config-manager/sync.enc` snapshot is written; the selected branch's other files are preserved. Encryption and per-item merge rules are shared with GitHub. Push races refetch and merge, at most three attempts.

JSON `sync_save_settings { request }` and REST `POST /sync/settings` accept optional `provider` and `git_branch` alongside existing `repo`, `github_token`, `encryption_secret`. Tauri `sync_save_settings` adds optional `provider` and `gitBranch` arguments. Omitted fields use the defaults above. Custom Git does not require a GitHub token.

JSON `sync_test_repository { request }`, REST `POST /sync/test`, and Tauri `sync_test_repository { request }` test the supplied draft target without saving settings. The request is `{ "provider": "git", "repo": "ssh://git@example.com/team/configs.git", "git_branch": "main", "github_token": null }`; provider and branch have the same defaults. GitHub uses the supplied token or, when null, the saved token. No encryption secret is needed and no local configuration is read. Both backends require installed Git for this explicit test. A real read/list/fetch is followed by creating and deleting a unique temporary branch under `ai-config-manager-probe/`. The test never changes the selected sync branch. This verifies general repository write permission; protected-branch rules can still prevent sync on the selected branch.

Tauri `sync_create_repository { request }` creates a repository from the same draft without saving settings. The request is `{ "repo": "owner/name", "github_token": null }`; a bare `name` creates it under the authenticated account, `owner/name` uses `POST /orgs/{owner}/repos`, and a null token reuses the saved one. Repositories are always created private. The result is `{ "repo": "owner/name", "url": "https://github.com/owner/name", "private": true }`. This desktop convenience has no REST/JSON agent equivalent and never persists the repository or token.

The result is `{ "ok": true, "readable": true, "writable": true, "cleanup_ok": true, "code": "OK", "latency_ms": 123, "probe_ref": null }`. A failed network/permission test returns those same fields with a fixed code and `ok: false`; invalid inputs and unavailable saved credentials may return protocol errors. If probe cleanup fails, `cleanup_ok` and `ok` are false and `probe_ref` identifies the branch requiring removal. Tests never return command output or credentials. Custom Git errors use `GIT_UNAVAILABLE`, `GIT_REPO`, `GIT_BRANCH`, `GIT_READ`, `GIT_WRITE`, `GIT_CONFLICT`, `GIT_TIMEOUT`, `GIT_CLEANUP`, `GIT_FAILED`.

The sidebar contains Sync list, Applications, and Sync records. Repository configuration and Automation remain settings tabs. Sync list displays only registered application configuration items, their enabled count, status and manual execution; AI/file items and the file registration form are not shown. Applications owns registration and callbacks, opening the registration form in a modal only on request. File registration remains available through the socket/SDK protocol.

The manager accepts authenticated JSON and REST on its existing AF_UNIX socket. A third-party application registers a stable application ID, application name, configuration ID and a local Unix socket callback. IDs identify the same configuration across devices; socket paths are local to each device and never restored from a remote bundle.

## Registration

`POST /sync/applications` or JSON `sync_register_application` with a `request`:

```json
{
  "app_id": "my-editor",
  "app_name": "My Editor",
  "config_id": "preferences",
  "label": "Editor preferences",
  "sync_interface": { "socket_path": "/home/alice/.config/my-editor/sync.sock", "path": "/config" },
  "enabled": true
}
```

Registration is an upsert by `app_id` + `config_id`, each 1–80 ASCII letters, digits, `-`, `_` or `.`. Registered configurations appear in `SyncView.items` with `kind: "application"`, `path: null`, `auto_sync: true`, and `application: { app_id, app_name, config_id, sync_interface }`. Re-registration updates the name, label and local callback while preserving the manager's existing `enabled` and `auto_sync` choices and the next scheduled deadline. The request's `enabled` value is used only for a new registration. Existing AI/file items remain compatible and have `application: null`; missing `auto_sync` defaults to true when loading older state. Removing or toggling an application configuration uses the existing item routes.

The executable Python AF_UNIX client in [register_application.py](../examples/register_application.py) registers an already running application callback without printing its local bearer token. For example: `python examples/register_application.py my-editor "My Editor" /home/alice/.config/my-editor/sync.sock --config-id preferences --label "Editor preferences"`. Registration itself does not perform a network sync.

The manager calls the registered socket with HTTP `GET <path>`. A successful response is JSON `{ "content": "<base64 bytes>", "updated_at": "<RFC3339 timestamp>" }`. Configuration size is limited to 1 MiB. To apply a newer remote configuration, the manager sends `PUT <path>` with the same object and an additional `expected_sha256` for the previously read bytes; the application must compare that digest and reject a concurrent edit with HTTP 409. The callback replies with 200 or 204 after committing. It must preserve the supplied update timestamp. No manager or GitHub token is forwarded to the callback.

## Application manual and automatic synchronization

These operations use the same local bearer-token authentication and shared `SyncStore` as the existing global operations. REST replies use `{ "ok": true, "data": <SyncView> }`; JSON operations sit inside the version-1 request's `op` object and return the same envelope. Responses include `Cache-Control: no-store` on REST.

| Operation | JSON `op` object | REST |
| --- | --- | --- |
| Sync an application now | `{ "op": "sync_run_application", "app_id": "my-editor" }` | `POST /sync/applications/my-editor/run` |
| Set automatic participation | `{ "op": "sync_set_application_auto_sync", "app_id": "my-editor", "enabled": false }` | `PATCH /sync/applications/my-editor/automation` with `{ "enabled": false }` |

An application manual run selects only that application's enabled registered items, regardless of `auto_sync`. It uses the existing bidirectional merge and callback apply flow, preserves unrelated remote entries, and leaves the global scheduled deadline unchanged on success or failure. The existing JSON `sync_run` / `POST /sync/run` continues to sync all enabled items. Calls from the socket use record trigger `agent`; a desktop manual action uses `manual`.

The automation operation updates `auto_sync` on all currently registered items for the application, including disabled items. It does not change `enabled`, the global policy, or the next deadline. Scheduled runs require both `enabled` and `auto_sync`; the global `policy.auto_sync_enabled` still controls whether the desktop scheduler runs. Manual actions remain available when automatic participation is off. Manager and application controls read and update this same persisted state.

Both operations validate the same application ID format as registration. Malformed IDs return `SYNC_APPLICATION` (REST 400); valid but unregistered IDs return `SYNC_ITEM_MISSING` (404). A manual run with all of the application's items disabled returns `SYNC_DISABLED` (400). Invalid automation bodies, including non-boolean `enabled`, return `INVALID_ARGUMENT` (400). Missing or incorrect bearer tokens return `UNAUTHORIZED` (401) before any mutation. Errors contain fixed messages and codes, never request payloads or credentials.

Python `AgentClient.sync_run_application(app_id, timeout=600)` and `sync_set_application_auto_sync(app_id, enabled)` expose the JSON operations. `sync_run()` retains its global behavior. The application manual run has its own timeout, like global sync; a timeout can leave a committed or still-running attempt, so clients must inspect state/history instead of blindly retrying. The shared Rust bridge and adapter lifecycle are specified in [application-integration.md](application-integration.md).

## Policy and records

`SyncView` adds:

```json
{
  "policy": { "auto_sync_enabled": true, "sync_interval_minutes": 10, "history_retention_days": 30 },
  "next_sync_at": null,
  "records": [],
  "deferred": []
}
```

`POST /sync/policy` / JSON `sync_save_policy { policy }` / Tauri `sync_save_policy { policy }` saves that policy. The interval is 1–1440 minutes; retention is 0–3650 days, with 0 disabling age-based deletion. The default policy enables automatic sync every 10 minutes. Scheduled attempts advance the deadline without recording a failure while no repository is saved, then run once a target exists. The newest 100 sync records are always retained at most. Each record has `id`, `started_at`, `completed_at`, `trigger` (`manual`, `agent`, `scheduled`), `state` (`success`, `error`), `repo`, `item_count`, `uploaded_count`, `downloaded_count`, `conflict_count`, `error_code` (nullable), `remote_sha` (nullable), and `app_id` (nullable). Application manual attempts record their `app_id`; global and scheduled attempts use null. Older records may omit `app_id`, which is read as null. No configuration contents or credentials are recorded. `GET /sync/history` / JSON `sync_history` / Tauri `sync_history` returns records, newest first.

A successful AI configuration save writes a `.dirty` marker beside the live configuration. On its next sweep the desktop scheduler turns it into a two-second debounce request only when automatic sync is enabled, a repository is saved, and the AI item participates automatically; otherwise the marker remains for a later sweep. A request raised during an active sync is preserved as `pending_sync`, so a follow-up attempt starts soon after completion instead of waiting the full interval. The marker and pending flag contain no configuration or credential data.

Startup, periodic maintenance and completed syncs prune excess/expired records. The same age policy removes expired AI configuration history through the store's existing lock/recovery path; its existing ten-entry limit is preserved. `POST /sync/history/cleanup` / JSON `sync_cleanup` / Tauri `sync_cleanup` runs cleanup immediately and returns the updated `SyncView`.

Tauri adds `sync_register_application { request }`. The sidebar owns Sync list, Applications and Sync records; settings tabs are General, Sync target, Automation and About. Drafts survive navigation within those management pages. Automatic sync runs only while the desktop process is running, including tray mode. Saving a policy resets the next deadline; global manual and scheduled attempts schedule the next attempt, including failures, so unavailable services cannot cause a tight retry loop. Application manual attempts preserve the global deadline.

## Offline applications and transient failures

A registered application that is not running, has removed its socket, or times out is reported as `SYNC_APPLICATION_OFFLINE` (REST 503) rather than failing the whole attempt. Its local snapshot is skipped, the remote copy is left untouched, and the item appears in `SyncView.deferred` as `{ "id", "label", "kind", "reason" }`. The same applies when applying a newer remote configuration fails for an application (`SYNC_CALLBACK`, `SYNC_LOCAL_CHANGED`, `SYNC_APPLICATION_OFFLINE`); file and AI items keep their existing fail-fast behavior because a partial local write would be visible to the user.

After a global manual or scheduled attempt succeeds with deferred items, or encounters a transient backend failure (`GITHUB_NETWORK`, `GITHUB_TIMEOUT`, `GITHUB_SERVER`, `GIT_TIMEOUT`, `GIT_READ`, `GIT_WRITE`, `GIT_FAILED`, `SYNC_VAULT`), the next attempt is scheduled with a 1-minute backoff that doubles with each consecutive failure (1, 2, 4, 8 minutes) and is capped by `sync_interval_minutes`. An interrupted previous attempt also retries on the short deadline. Deferred entries clear on the next attempt in which the item syncs, and are dropped when the item is disabled, removed, or its registration changes. Turning off an application's automatic participation also clears its deferred entries. A permanently rejected target (`GITHUB_AUTH`, `SYNC_REPO`, `SYNC_PASSWORD`, `SYNC_NOT_CONFIGURED`) keeps the configured interval. Application manual attempts do not change that scheduling state.

An application with automatic participation off is never added to `deferred`. If its selected item fails during an application-scoped manual run, that attempt record carries `state: "error"` and the fixed code, so the application can report the failure without any automatic retry being scheduled. Global runs do not attribute such a failure to unrelated applications.

## Conflict resolution

Both backends decrypt the current remote bundle and merge each configuration by stable ID. The configuration with the later `updated_at` wins. File/AI timestamps come from the file modification time; application timestamps come from the callback. Equal timestamps use the content digest as a deterministic tie-breaker. Remote-only entries are preserved, and disabled configurations are not applied locally. Older v1 entries without per-item timestamps inherit the bundle creation time.

A successful upload is followed by applying remote winners only to locally registered configurations selected for that attempt: enabled items for global manual runs, enabled items belonging to the requested application for application manual runs, and `enabled && auto_sync` items for scheduled runs. Concurrent local edits are checked again and never blindly replaced. AI imports retain encrypted local storage and use revision checks and the history journal. GitHub 409/422 write races and custom Git branch lease conflicts trigger bounded refetch/merge/retry (at most three attempts). An invalid envelope, wrong password or malformed remote data fails the attempt; an application callback that is offline or rejects the update is deferred instead of failing the whole attempt. Device clocks must be accurate for timestamp ordering.
