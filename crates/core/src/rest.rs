use crate::agent::{AgentSyncSettings, RestRequest, RestResponse};
use crate::sync::{
    ApplicationRegistration, RepositoryTestRequest, SyncPolicy, SyncStore, SyncTrigger,
};
use crate::{EndpointKind, Error, SaveRequest, Store};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub fn handle_rest(store: &Store, sync: &SyncStore, request: &RestRequest) -> RestResponse {
    if let Some(rest) = request.path.strip_prefix("/ai-config/endpoints/") {
        let mut parts = rest.split('/');
        let Some(kind) = parts.next().and_then(EndpointKind::from_key) else {
            return fail(not_found());
        };
        let tail: Vec<_> = parts.filter(|part| !part.is_empty()).collect();
        return match (request.method.as_str(), tail.as_slice()) {
            ("GET", []) => store.load_endpoint(kind).map(ok).unwrap_or_else(fail),
            ("GET", ["decrypted"]) => crate::agent::decrypted_endpoint_response(store, kind),
            ("POST", ["test"]) => store.test_endpoint_kind(kind).map(ok).unwrap_or_else(fail),
            ("POST", ["check"]) => store
                .validate_endpoint(kind)
                .map(|_| ok(json!({ "valid": true })))
                .unwrap_or_else(fail),
            ("POST", []) => match body::<SaveRequest>(request) {
                Ok(save) => store
                    .save_endpoint(kind, save, "skill")
                    .map(ok)
                    .unwrap_or_else(fail),
                Err(error) => fail(error),
            },
            _ => fail(not_found()),
        };
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/health") => ok(json!({
            "service": "ai-config-manager",
            "version": env!("CARGO_PKG_VERSION")
        })),
        ("GET", "/ai-config") => store.load().map(ok).unwrap_or_else(fail),
        ("GET", "/ai-config/decrypted") => crate::agent::decrypted_config_response(store),
        ("GET", "/ai-config/history") => store.history().map(ok).unwrap_or_else(fail),
        ("GET", "/sync") => ok(sync.view()),
        ("GET", "/sync/status") => ok(sync.view().status),
        ("POST", "/sync/settings") => match body::<AgentSyncSettings>(request) {
            Ok(mut settings) => sync
                .save_repository_settings(
                    settings.provider,
                    &settings.repo,
                    &settings.git_branch,
                    settings.github_token.take(),
                    settings.encryption_secret.take(),
                )
                .map(ok)
                .unwrap_or_else(fail),
            Err(error) => fail(error),
        },
        ("POST", "/sync/items") => match body::<ItemBody>(request) {
            Ok(item) => sync
                .add_item(item.label, &item.path, item.enabled)
                .map(ok)
                .unwrap_or_else(fail),
            Err(error) => fail(error),
        },
        ("POST", "/sync/run") => sync
            .run_sync_with_trigger(SyncTrigger::Agent)
            .map(ok)
            .unwrap_or_else(fail),
        ("POST", "/sync/test") => match body::<RepositoryTestRequest>(request) {
            Ok(target) => sync.test_repository(target).map(ok).unwrap_or_else(fail),
            Err(error) => fail(error),
        },
        ("POST", "/sync/applications") => match body::<ApplicationRegistration>(request) {
            Ok(registration) => sync
                .register_application(registration)
                .map(ok)
                .unwrap_or_else(fail),
            Err(error) => fail(error),
        },
        ("POST", path)
            if path
                .strip_prefix("/sync/applications/")
                .is_some_and(|tail| tail.ends_with("/run")) =>
        {
            let app_id = &path["/sync/applications/".len()..path.len() - "/run".len()];
            sync.run_application(app_id, SyncTrigger::Agent)
                .map(ok)
                .unwrap_or_else(fail)
        }
        ("PATCH", path)
            if path
                .strip_prefix("/sync/applications/")
                .is_some_and(|tail| tail.ends_with("/automation")) =>
        {
            let app_id = &path["/sync/applications/".len()..path.len() - "/automation".len()];
            match body::<ItemEnabledBody>(request) {
                Ok(item) => sync
                    .set_application_auto_sync(app_id, item.enabled)
                    .map(ok)
                    .unwrap_or_else(fail),
                Err(error) => fail(error),
            }
        }
        ("GET", "/sync/applications") => ok(sync
            .view()
            .items
            .into_iter()
            .filter(|item| item.application.is_some())
            .collect::<Vec<_>>()),
        ("POST", "/sync/policy") => match body::<SyncPolicy>(request) {
            Ok(policy) => sync.save_policy(policy).map(ok).unwrap_or_else(fail),
            Err(error) => fail(error),
        },
        ("GET", "/sync/history") => sync.history().map(ok).unwrap_or_else(fail),
        ("POST", "/sync/history/cleanup") => sync.cleanup().map(ok).unwrap_or_else(fail),
        ("PATCH", path) if path.starts_with("/sync/items/") => {
            let id = path.trim_start_matches("/sync/items/");
            if id.is_empty() {
                return fail(not_found());
            }
            match body::<ItemEnabledBody>(request) {
                Ok(item) => sync
                    .set_item_enabled(id, item.enabled)
                    .map(ok)
                    .unwrap_or_else(fail),
                Err(error) => fail(error),
            }
        }
        ("DELETE", path) if path.starts_with("/sync/items/") => {
            let id = path.trim_start_matches("/sync/items/");
            if id.is_empty() {
                return fail(not_found());
            }
            sync.remove_item(id).map(ok).unwrap_or_else(fail)
        }
        _ => fail(not_found()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemBody {
    #[serde(default)]
    label: Option<String>,
    path: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemEnabledBody {
    enabled: bool,
}

fn default_true() -> bool {
    true
}

fn body<T: DeserializeOwned>(request: &RestRequest) -> Result<T, Error> {
    serde_json::from_slice(&request.body).map_err(|_| Error {
        code: "INVALID_ARGUMENT".into(),
        message: "The REST request body is not valid JSON for this endpoint.".into(),
    })
}

fn ok<T: Serialize>(data: T) -> RestResponse {
    RestResponse::json(200, &json!({ "ok": true, "data": data }))
}

fn fail(error: Error) -> RestResponse {
    let status = match error.code.as_str() {
        "SYNC_BUSY" | "GITHUB_CONFLICT" | "GIT_CONFLICT" | "SYNC_LOCAL_CHANGED" => 409,
        "NOT_FOUND" | "SYNC_ITEM_MISSING" | "GITHUB_REPO_NOT_FOUND" => 404,
        "SYNC_VAULT" | "GITHUB_SERVER" | "GITHUB_NETWORK" | "GITHUB_TIMEOUT"
        | "GITHUB_RATE_LIMIT" | "IO" => 502,
        "SYNC_APPLICATION_OFFLINE" => 503,
        "INVALID_ARGUMENT"
        | "SYNC_CONFIG"
        | "SYNC_REPO"
        | "SYNC_TOKEN"
        | "SYNC_PASSWORD"
        | "SYNC_NOT_CONFIGURED"
        | "SYNC_DISABLED"
        | "SYNC_PATH"
        | "SYNC_DUPLICATE"
        | "SYNC_FILE_TOO_LARGE"
        | "SYNC_INTERFACE"
        | "SYNC_APPLICATION"
        | "SYNC_POLICY"
        | "GIT_REPO"
        | "GIT_BRANCH"
        | "GITHUB_AUTH"
        | "GITHUB_FORBIDDEN"
        | "GITHUB_REJECTED" => 400,
        _ => 500,
    };
    RestResponse::error(status, &error)
}

fn not_found() -> Error {
    Error {
        code: "NOT_FOUND".into(),
        message: "The requested REST endpoint does not exist.".into(),
    }
}
