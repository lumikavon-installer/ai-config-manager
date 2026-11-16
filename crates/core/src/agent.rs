//! Local agent protocol used by the desktop app and the `ai-config` CLI.
//!
//! On Unix this is a Unix domain socket. On Windows it is an AF_UNIX socket
//! through `uds_windows`, so both platforms use the same protocol and the same
//! filesystem-path naming rules. A private bearer token is written next to the
//! socket and must accompany every request; the token is never printed.

use super::*;
use crate::sync::{
    default_git_branch, ApplicationRegistration, RepositoryTestRequest, SyncPolicy, SyncProvider,
    SyncStore, SyncTrigger,
};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub const AGENT_VERSION: u32 = 1;
const MAX_MESSAGE: u64 = 2 * 1024 * 1024;
const TOKEN_CHARS: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const SYNC_TIMEOUT: Duration = Duration::from_secs(600);

#[cfg(unix)]
type Listener = std::os::unix::net::UnixListener;
#[cfg(windows)]
type Listener = uds_windows::UnixListener;
#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = uds_windows::UnixStream;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPaths {
    pub socket: PathBuf,
    pub token: PathBuf,
}

#[derive(Serialize, Deserialize)]
pub struct AgentRequest {
    pub version: u32,
    pub token: String,
    pub op: AgentOp,
}
impl Drop for AgentRequest {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

/// Wire representation for a save. Kept separate from `SaveRequest` so the
/// core request type never implements `Serialize`, which would make it too easy
/// to accidentally print or log a credential.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSaveRequest {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    #[serde(default = "default_true")]
    pub encrypt: bool,
    pub expected_revision: String,
}
impl AgentSaveRequest {
    pub fn into_save(mut self) -> SaveRequest {
        SaveRequest {
            base_url: std::mem::take(&mut self.base_url),
            model: std::mem::take(&mut self.model),
            api_key: self.api_key.take(),
            encrypt: self.encrypt,
            expected_revision: std::mem::take(&mut self.expected_revision),
        }
    }
}
impl Drop for AgentSaveRequest {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSyncSettings {
    #[serde(default)]
    pub provider: SyncProvider,
    #[serde(default = "default_git_branch")]
    pub git_branch: String,
    pub repo: String,
    pub github_token: Option<String>,
    pub encryption_secret: Option<String>,
}
impl Drop for AgentSyncSettings {
    fn drop(&mut self) {
        self.github_token.zeroize();
        self.encryption_secret.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AgentOp {
    Ping,
    Read,
    /// Explicit credential-bearing read for authenticated, trusted local SDKs.
    ReadDecrypted,
    ReadEndpoint {
        kind: EndpointKind,
    },
    ReadEndpointDecrypted {
        kind: EndpointKind,
    },
    Check,
    History,
    Test,
    CheckEndpoint {
        kind: EndpointKind,
    },
    TestEndpoint {
        kind: EndpointKind,
    },
    Save {
        request: AgentSaveRequest,
    },
    SaveEndpoint {
        kind: EndpointKind,
        request: AgentSaveRequest,
    },
    SyncRead,
    SyncSaveSettings {
        request: AgentSyncSettings,
    },
    SyncAddItem {
        label: Option<String>,
        path: String,
        #[serde(default = "default_true")]
        enabled: bool,
    },
    SyncSetItemEnabled {
        id: String,
        enabled: bool,
    },
    SyncRemoveItem {
        id: String,
    },
    SyncRun,
    SyncRunApplication {
        app_id: String,
    },
    SyncSetApplicationAutoSync {
        app_id: String,
        enabled: bool,
    },
    SyncRegisterApplication {
        request: ApplicationRegistration,
    },
    SyncSavePolicy {
        policy: SyncPolicy,
    },
    SyncHistory,
    SyncCleanup,
    SyncTestRepository {
        request: RepositoryTestRequest,
    },
}

/// Shared dispatch for the desktop socket and trusted in-process integrations.
pub fn dispatch(store: &Store, sync: &SyncStore, op: AgentOp) -> Result<serde_json::Value> {
    fn encode(value: impl Serialize) -> Result<serde_json::Value> {
        serde_json::to_value(value).map_err(|_| err("AGENT_PROTOCOL"))
    }
    match op {
        AgentOp::Ping => Ok(serde_json::json!({ "version": AGENT_VERSION })),
        AgentOp::Read => encode(store.load()?),
        AgentOp::ReadDecrypted => read_decrypted_config(store),
        AgentOp::ReadEndpoint { kind } => encode(store.load_endpoint(kind)?),
        AgentOp::ReadEndpointDecrypted { kind } => read_decrypted_endpoint(store, kind),
        AgentOp::Check => {
            store.validate_secret()?;
            Ok(serde_json::json!({ "valid": true }))
        }
        AgentOp::History => encode(store.history()?),
        AgentOp::Test => encode(store.test_endpoint()?),
        AgentOp::CheckEndpoint { kind } => {
            store.validate_endpoint(kind)?;
            Ok(serde_json::json!({ "valid": true }))
        }
        AgentOp::TestEndpoint { kind } => encode(store.test_endpoint_kind(kind)?),
        AgentOp::Save { request } => encode(store.save_with_source(request.into_save(), "skill")?),
        AgentOp::SaveEndpoint { kind, request } => {
            encode(store.save_endpoint(kind, request.into_save(), "skill")?)
        }
        AgentOp::SyncRead => encode(sync.view()),
        AgentOp::SyncSaveSettings { mut request } => encode(sync.save_repository_settings(
            request.provider,
            &request.repo,
            &request.git_branch,
            request.github_token.take(),
            request.encryption_secret.take(),
        )?),
        AgentOp::SyncAddItem {
            label,
            path,
            enabled,
        } => encode(sync.add_item(label, &path, enabled)?),
        AgentOp::SyncSetItemEnabled { id, enabled } => encode(sync.set_item_enabled(&id, enabled)?),
        AgentOp::SyncRemoveItem { id } => encode(sync.remove_item(&id)?),
        AgentOp::SyncRun => encode(sync.run_sync_with_trigger(SyncTrigger::Agent)?),
        AgentOp::SyncRunApplication { app_id } => {
            encode(sync.run_application(&app_id, SyncTrigger::Agent)?)
        }
        AgentOp::SyncSetApplicationAutoSync { app_id, enabled } => {
            encode(sync.set_application_auto_sync(&app_id, enabled)?)
        }
        AgentOp::SyncRegisterApplication { request } => encode(sync.register_application(request)?),
        AgentOp::SyncSavePolicy { policy } => encode(sync.save_policy(policy)?),
        AgentOp::SyncHistory => encode(sync.history()?),
        AgentOp::SyncCleanup => encode(sync.cleanup()?),
        AgentOp::SyncTestRepository { request } => encode(sync.test_repository(request)?),
    }
}

#[derive(Serialize, Deserialize)]
pub struct AgentReply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}
impl AgentReply {
    pub fn into_result(mut self) -> Result<serde_json::Value> {
        if self.ok {
            Ok(self.data.take().unwrap_or(serde_json::Value::Null))
        } else {
            Err(self.error.take().unwrap_or_else(|| err("AGENT_PROTOCOL")))
        }
    }
}
impl Drop for AgentReply {
    fn drop(&mut self) {
        if let Some(data) = self.data.as_mut() {
            config::wipe_json(data);
        }
    }
}

/// Credential-bearing wire data, only for the authenticated local socket.
/// Secret and ConfigSnapshot deliberately remain non-serializable.
pub fn read_decrypted_config(store: &Store) -> Result<serde_json::Value> {
    read_decrypted_endpoint(store, EndpointKind::Openai)
}

pub fn read_decrypted_endpoint(store: &Store, kind: EndpointKind) -> Result<serde_json::Value> {
    store.with_endpoint(kind, |snapshot| {
        snapshot.with_secret(|key| {
            serde_json::json!({
                "base_url": snapshot.base_url,
                "model": snapshot.model,
                "api_key": key,
                "encrypted": snapshot.encrypted,
                "endpoint": snapshot.endpoint.key(),
            })
        })
    })
}

pub fn decrypted_endpoint_response(store: &Store, kind: EndpointKind) -> RestResponse {
    match read_decrypted_endpoint(store, kind) {
        Ok(data) => RestResponse::json(
            200,
            &AgentReply {
                ok: true,
                data: Some(data),
                error: None,
            },
        ),
        Err(error) => RestResponse::error(500, &error),
    }
}

pub fn decrypted_config_response(store: &Store) -> RestResponse {
    match read_decrypted_config(store) {
        Ok(data) => RestResponse::json(
            200,
            &AgentReply {
                ok: true,
                data: Some(data),
                error: None,
            },
        ),
        Err(error) => RestResponse::error(500, &error),
    }
}

/// The CLI must fall back to direct file access only when the desktop app is
/// demonstrably absent. Once a request has been written to a connected socket,
/// failures are reported as failures: a blind direct retry could duplicate a
/// committed save whose response was lost.
pub enum AgentCall {
    Unavailable,
    Reply(AgentReply),
    Failure(Error),
}

const MAX_HTTP_HEADERS: usize = 16 * 1024;
const MAX_HTTP_BODY: u64 = 1024 * 1024;

pub struct RestRequest {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}
impl Drop for RestRequest {
    fn drop(&mut self) {
        self.body.zeroize();
        for value in self.headers.values_mut() {
            value.zeroize();
        }
    }
}

pub struct RestResponse {
    pub status: u16,
    pub body: Vec<u8>,
}
impl Drop for RestResponse {
    fn drop(&mut self) {
        self.body.zeroize();
    }
}
impl RestResponse {
    pub fn json<T: Serialize>(status: u16, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(body) => Self { status, body },
            Err(_) => Self {
                status: 500,
                body: br#"{"ok":false,"error":{"code":"SERIALIZE","message":"Cannot encode the response."}}"#.to_vec(),
            },
        }
    }
    pub fn error(status: u16, error: &Error) -> Self {
        Self::json(status, &serde_json::json!({"ok": false, "error": error}))
    }
}

pub fn paths() -> Result<AgentPaths> {
    let home = dirs::home_dir().ok_or_else(|| err("IO"))?;
    let dir = home.join(".config").join("ai-config-manager");
    let preferred = dir.join("agent.sock");
    // AF_UNIX paths are short (108 bytes on Windows and typically 108 on
    // Unix). Fall back to a deterministic per-user temp path when the home
    // directory is unusually long; both the app and CLI compute the same path.
    let socket = if preferred.to_string_lossy().len() <= 100 {
        preferred
    } else {
        let digest = hash(home.to_string_lossy().as_bytes());
        // Use a dedicated subdirectory, never chmod the shared system temp
        // directory itself. Both the app and CLI derive the same path.
        let temp = std::env::temp_dir();
        let temp = temp.canonicalize().unwrap_or(temp);
        temp.join(format!("ai-config-manager-{}", &digest[..16]))
            .join("agent.sock")
    };
    Ok(AgentPaths {
        socket,
        token: dir.join("agent.token"),
    })
}

pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn write_token(path: &Path, token: &str) -> Result<()> {
    if token.len() != TOKEN_CHARS || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(err("AGENT_TOKEN"));
    }
    let parent = path.parent().ok_or_else(|| err("UNSAFE_PATH"))?;
    safe_ancestors(parent)?;
    fs::create_dir_all(parent).map_err(|_| err("IO"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|_| err("IO"))?;
    }
    safe_file(path)?;
    let mut file = platform::open_private(path, false)?;
    file.set_len(0).map_err(|_| err("IO"))?;
    file.write_all(token.as_bytes()).map_err(|_| err("IO"))?;
    file.sync_all().map_err(|_| err("IO"))?;
    Ok(())
}

pub fn read_token(path: &Path) -> Result<String> {
    safe_file(path)?;
    let file = platform::open_read(path).map_err(|_| err("AGENT_UNAVAILABLE"))?;
    let mut token = String::new();
    file.take(TOKEN_CHARS as u64 + 1)
        .read_to_string(&mut token)
        .map_err(|_| err("AGENT_UNAVAILABLE"))?;
    if token.len() != TOKEN_CHARS || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        token.zeroize();
        return Err(err("AGENT_TOKEN"));
    }
    Ok(token)
}

pub fn bind(socket: &Path) -> Result<Listener> {
    let parent = socket.parent().ok_or_else(|| err("UNSAFE_PATH"))?;
    safe_ancestors(parent)?;
    fs::create_dir_all(parent).map_err(|_| err("IO"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|_| err("IO"))?;
    }
    match fs::symlink_metadata(socket) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(err("UNSAFE_PATH"));
        }
        Ok(_) => {
            if Stream::connect(socket).is_ok() {
                return Err(err("AGENT_IN_USE"));
            }
            fs::remove_file(socket).map_err(|_| err("IO"))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(err("IO")),
    }
    let listener = Listener::bind(socket).map_err(|_| err("AGENT_UNAVAILABLE"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600)).map_err(|_| err("IO"))?;
    }
    Ok(listener)
}

pub fn serve_stream<S, F>(stream: S, expected_token: &str, handler: &F) -> std::io::Result<()>
where
    S: Read + Write,
    F: Fn(AgentOp) -> Result<serde_json::Value>,
{
    serve_stream_with_rest(stream, expected_token, handler, &|_| {
        RestResponse::error(404, &err("INVALID_ARGUMENT"))
    })
}

/// Serve both the legacy newline JSON protocol used by `ai-config` and an HTTP
/// REST interface used by integrations. The first line selects the protocol:
/// JSON starts with `{`, HTTP starts with a method token.
pub fn serve_stream_with_rest<S, F, R>(
    mut stream: S,
    expected_token: &str,
    json_handler: &F,
    rest_handler: &R,
) -> std::io::Result<()>
where
    S: Read + Write,
    F: Fn(AgentOp) -> Result<serde_json::Value>,
    R: Fn(RestRequest) -> RestResponse,
{
    let mut first = Zeroizing::new(String::new());
    let mut reader = BufReader::new(&mut stream);
    {
        let mut limited = reader.by_ref().take(MAX_MESSAGE + 1);
        limited.read_line(&mut first)?;
    }
    if first.len() as u64 > MAX_MESSAGE {
        first.zeroize();
        drop(reader);
        write_reply(&mut stream, error_reply("TOO_LARGE"))?;
        return Ok(());
    }
    if is_http_request_line(&first) {
        let response = handle_http_request(&mut reader, &first, expected_token, rest_handler)?;
        drop(reader);
        write_http_response(&mut stream, response)
    } else {
        let reply = handle_json_line(&first, expected_token, json_handler);
        first.zeroize();
        drop(reader);
        write_reply(&mut stream, reply)
    }
}

fn is_http_request_line(line: &str) -> bool {
    let mut parts = line.split_whitespace();
    matches!(
        parts.next(),
        Some("GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS")
    ) && parts.next().is_some()
}

fn handle_json_line<F>(line: &str, expected_token: &str, handler: &F) -> AgentReply
where
    F: Fn(AgentOp) -> Result<serde_json::Value>,
{
    if line.trim().is_empty() {
        return error_reply("INVALID_ARGUMENT");
    }
    match serde_json::from_str::<AgentRequest>(line) {
        Ok(request) if request.version != AGENT_VERSION => error_reply("AGENT_PROTOCOL"),
        Ok(request) if !token_matches(request.token.as_bytes(), expected_token.as_bytes()) => {
            error_reply("UNAUTHORIZED")
        }
        Ok(mut request) => {
            let op = std::mem::replace(&mut request.op, AgentOp::Ping);
            match handler(op) {
                Ok(data) => AgentReply {
                    ok: true,
                    data: Some(data),
                    error: None,
                },
                Err(error) => AgentReply {
                    ok: false,
                    data: None,
                    error: Some(error),
                },
            }
        }
        Err(_) => error_reply("INVALID_ARGUMENT"),
    }
}

fn handle_http_request<B, R>(
    reader: &mut B,
    first: &str,
    expected_token: &str,
    handler: &R,
) -> std::io::Result<RestResponse>
where
    B: BufRead,
    R: Fn(RestRequest) -> RestResponse,
{
    let request_line = first.trim_end_matches(['\r', '\n']);
    let mut parts = request_line.split_whitespace();
    let method = match parts.next() {
        Some(method) => method.to_string(),
        None => return Ok(RestResponse::error(400, &err("INVALID_ARGUMENT"))),
    };
    let target = match parts.next() {
        Some(target) => target,
        None => return Ok(RestResponse::error(400, &err("INVALID_ARGUMENT"))),
    };
    match parts.next() {
        Some(version) if version.starts_with("HTTP/") => {}
        _ => return Ok(RestResponse::error(400, &err("INVALID_ARGUMENT"))),
    }
    if parts.next().is_some() {
        return Ok(RestResponse::error(400, &err("INVALID_ARGUMENT")));
    }
    let mut request = RestRequest {
        method,
        path: target.split(['?', '#']).next().unwrap_or(target).into(),
        headers: HashMap::new(),
        body: Vec::new(),
    };
    let mut header_bytes = first.len();
    loop {
        let mut line = Zeroizing::new(String::new());
        let read = reader
            .by_ref()
            .take(MAX_HTTP_HEADERS.saturating_sub(header_bytes) as u64 + 1)
            .read_line(&mut line)?;
        if read == 0 {
            return Ok(RestResponse::error(400, &err("INVALID_ARGUMENT")));
        }
        header_bytes = header_bytes.saturating_add(read);
        if header_bytes > MAX_HTTP_HEADERS {
            return Ok(RestResponse::error(400, &err("INVALID_ARGUMENT")));
        }
        let line_text = line.trim_end_matches(['\r', '\n']);
        if line_text.is_empty() {
            line.zeroize();
            break;
        }
        let Some((name, value)) = line_text.split_once(':') else {
            line.zeroize();
            return Ok(RestResponse::error(400, &err("INVALID_ARGUMENT")));
        };
        let name = name.trim().to_ascii_lowercase();
        if request.headers.contains_key(&name) {
            return Ok(RestResponse::error(400, &err("INVALID_ARGUMENT")));
        }
        request.headers.insert(name, value.trim().into());
        line.zeroize();
    }
    if request.headers.contains_key("transfer-encoding") {
        return Ok(RestResponse::error(400, &err("INVALID_ARGUMENT")));
    }
    let content_length = match request.headers.get("content-length") {
        Some(value) => match value.parse::<u64>() {
            Ok(value) if value <= MAX_HTTP_BODY => value,
            Ok(_) => return Ok(RestResponse::error(413, &err("TOO_LARGE"))),
            Err(_) => return Ok(RestResponse::error(400, &err("INVALID_ARGUMENT"))),
        },
        None => 0,
    };
    let bearer = request
        .headers
        .get("authorization")
        .and_then(|value| bearer_value(value))
        .or_else(|| request.headers.get("x-agent-token").map(String::as_str));
    let authorized = bearer
        .map(|token| token_matches(token.as_bytes(), expected_token.as_bytes()))
        .unwrap_or(false);
    if !authorized {
        return Ok(RestResponse::error(401, &err("UNAUTHORIZED")));
    }
    request.body.resize(content_length as usize, 0);
    reader.read_exact(&mut request.body)?;
    Ok(handler(request))
}

fn bearer_value(value: &str) -> Option<&str> {
    let value = value.trim();
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
        Some(token.trim())
    } else {
        None
    }
}

fn write_http_response<W: Write>(stream: &mut W, response: RestResponse) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "Response",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        response.status,
        reason,
        response.body.len()
    )?;
    stream.write_all(&response.body)?;
    stream.flush()
}

fn write_reply<W: Write>(stream: &mut W, reply: AgentReply) -> std::io::Result<()> {
    serde_json::to_writer(&mut *stream, &reply)?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn error_reply(code: &str) -> AgentReply {
    AgentReply {
        ok: false,
        data: None,
        error: Some(err(code)),
    }
}

fn token_matches(received: &[u8], expected: &[u8]) -> bool {
    if received.len() != expected.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in received.iter().zip(expected) {
        difference |= left ^ right;
    }
    difference == 0
}

pub fn is_running() -> bool {
    matches!(
        try_request(AgentOp::Ping),
        AgentCall::Reply(AgentReply { ok: true, .. })
    )
}

pub fn try_request(op: AgentOp) -> AgentCall {
    let paths = match paths() {
        Ok(paths) => paths,
        Err(_) => return AgentCall::Unavailable,
    };
    let token = match read_token(&paths.token) {
        Ok(token) => token,
        Err(error) if error.code == "AGENT_TOKEN" && paths.socket.exists() => {
            return AgentCall::Failure(error)
        }
        Err(_) => return AgentCall::Unavailable,
    };
    match request_at(&paths.socket, &token, op) {
        Ok(reply) => AgentCall::Reply(reply),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AgentCall::Unavailable,
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            AgentCall::Unavailable
        }
        Err(_) => AgentCall::Failure(err("AGENT_PROTOCOL")),
    }
}

pub fn request_at(socket: &Path, token: &str, op: AgentOp) -> std::io::Result<AgentReply> {
    let mut stream = Stream::connect(socket)?;
    let timeout = if matches!(
        &op,
        AgentOp::SyncRun | AgentOp::SyncRunApplication { .. } | AgentOp::SyncTestRepository { .. }
    ) {
        SYNC_TIMEOUT
    } else {
        CONNECT_TIMEOUT
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(CONNECT_TIMEOUT));
    let request = AgentRequest {
        version: AGENT_VERSION,
        token: token.into(),
        op,
    };
    let mut encoded = Zeroizing::new(serde_json::to_vec(&request)?);
    encoded.push(b'\n');
    stream.write_all(&encoded)?;
    stream.flush()?;
    drop(request);
    encoded.zeroize();

    let mut reader = BufReader::new(stream);
    let mut response = Zeroizing::new(String::new());
    reader
        .by_ref()
        .take(MAX_MESSAGE + 1)
        .read_line(&mut response)?;
    if response.len() as u64 > MAX_MESSAGE {
        return Err(std::io::Error::other("agent response exceeds limit"));
    }
    let reply = serde_json::from_str(&response)
        .map_err(|_| std::io::Error::other("invalid agent response"))?;
    response.zeroize();
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::thread;

    #[test]
    fn token_comparison_is_length_checked_and_exact() {
        assert!(token_matches(b"abcd", b"abcd"));
        assert!(!token_matches(b"abcd", b"abce"));
        assert!(!token_matches(b"abc", b"abcd"));
    }

    #[test]
    fn rest_bounds_headers_and_rejects_ambiguous_framing_before_dispatch() {
        for headers in [
            format!("X-Long: {}\r\n\r\n", "x".repeat(MAX_HTTP_HEADERS + 1)),
            "Content-Length: 0\r\nContent-Length: 1\r\n\r\n".into(),
            "Transfer-Encoding: chunked\r\n\r\n".into(),
        ] {
            let mut reader = std::io::Cursor::new(headers.as_bytes());
            let response =
                handle_http_request(&mut reader, "POST /sync/run HTTP/1.1\r\n", "token", &|_| {
                    panic!("invalid request was dispatched")
                })
                .unwrap();
            assert_eq!(response.status, 400);
        }
        let mut reader = std::io::Cursor::new(b"Content-Length: 10\r\n\r\n");
        let response =
            handle_http_request(&mut reader, "POST /sync/run HTTP/1.1\r\n", "token", &|_| {
                panic!("unauthorized request was dispatched")
            })
            .unwrap();
        assert_eq!(response.status, 401);
    }

    #[test]
    fn protocol_dispatches_and_rejects_bad_tokens() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().canonicalize().unwrap().join("agent.sock");
        let token = new_token();
        let listener = bind(&socket).unwrap();
        let server_token = token.clone();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                serve_stream(stream, &server_token, &|op| match op {
                    AgentOp::Ping => Ok(json!({"version": AGENT_VERSION})),
                    _ => Err(err("INVALID_ARGUMENT")),
                })
                .unwrap();
            }
        });

        let reply = request_at(&socket, &token, AgentOp::Ping).unwrap();
        assert!(reply.ok);
        assert_eq!(reply.data.as_ref().unwrap()["version"], AGENT_VERSION);

        let rejected = request_at(&socket, &new_token(), AgentOp::Ping).unwrap();
        assert!(!rejected.ok);
        assert_eq!(rejected.error.as_ref().unwrap().code, "UNAUTHORIZED");
        server.join().unwrap();
    }

    #[test]
    fn rest_requests_use_bearer_auth_and_dispatch_methods() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().canonicalize().unwrap().join("rest.sock");
        let token = new_token();
        let listener = bind(&socket).unwrap();
        let server_token = token.clone();
        let server = thread::spawn(move || {
            for _ in 0..3 {
                let (stream, _) = listener.accept().unwrap();
                serve_stream_with_rest(
                    stream,
                    &server_token,
                    &|op| match op {
                        AgentOp::Ping => Ok(json!({"version": AGENT_VERSION})),
                        _ => Err(err("INVALID_ARGUMENT")),
                    },
                    &|request| {
                        RestResponse::json(
                            200,
                            &json!({
                                "method": request.method,
                                "path": request.path,
                                "body_len": request.body.len(),
                            }),
                        )
                    },
                )
                .unwrap();
            }
        });
        let unauthorized = raw_http(&socket, "GET /health HTTP/1.1\r\nHost: local\r\n\r\n");
        assert!(unauthorized.starts_with("HTTP/1.1 401"));
        let empty = raw_http(&socket, "GET /health HTTP/1.1\r\nHost: local\r\n\r\n");
        assert!(empty.starts_with("HTTP/1.1 401"));
        let body = r#"{"path":"C:\\tmp\\x"}"#;
        let request = format!(
            "POST /sync/items HTTP/1.1\r\nHost: local\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let authorized = raw_http(&socket, &request);
        assert!(authorized.starts_with("HTTP/1.1 200"));
        assert!(authorized.contains(r#""method":"POST""#));
        assert!(authorized.contains(r#""path":"/sync/items""#));
        assert!(authorized.contains(&format!(r#""body_len":{}"#, body.len())));
        server.join().unwrap();
    }

    fn raw_http(socket: &Path, request: &str) -> String {
        let mut stream = Stream::connect(socket).unwrap();
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        stream.write_all(request.as_bytes()).unwrap();
        stream.flush().unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }
}
