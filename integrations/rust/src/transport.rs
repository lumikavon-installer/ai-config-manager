use crate::{err, fixed_error, platform, Result, Sensitive, Shared, MAX_CONTENT};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use socket2::{Domain, SockAddr, Socket, Type};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{Read, Write},
    net::Shutdown,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

#[cfg(unix)]
pub(crate) type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
pub(crate) type Stream = uds_windows::UnixStream;
#[cfg(unix)]
pub(crate) type Listener = std::os::unix::net::UnixListener;
#[cfg(windows)]
pub(crate) type Listener = uds_windows::UnixListener;

const MAX_HEADERS: usize = 16 * 1024;
const MAX_BODY: usize = 2 * 1024 * 1024;
const IO_TICK: Duration = Duration::from_millis(10);
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const SYNC_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub(crate) struct ManagerPaths {
    pub(crate) socket: PathBuf,
    pub(crate) token: PathBuf,
}

pub(crate) fn manager_paths() -> Result<ManagerPaths> {
    let home = dirs::home_dir().ok_or_else(|| err("AGENT_UNAVAILABLE"))?;
    let directory = home.join(".config").join("ai-config-manager");
    let preferred = directory.join("agent.sock");
    let socket = if preferred.to_string_lossy().len() <= 100 {
        preferred
    } else {
        let digest = crate::hash(home.to_string_lossy().as_bytes());
        let temp = std::env::temp_dir();
        let temp = temp.canonicalize().unwrap_or(temp);
        temp.join(format!("ai-config-manager-{}", &digest[..16]))
            .join("agent.sock")
    };
    Ok(ManagerPaths {
        socket,
        token: directory.join("agent.token"),
    })
}

#[derive(Default)]
pub(crate) struct Control {
    stopped: AtomicBool,
    wake: Condvar,
    wait_lock: Mutex<()>,
    active: Mutex<BTreeMap<u64, Stream>>,
    next: AtomicU64,
}

impl Control {
    pub(crate) fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    pub(crate) fn stop(&self) {
        let _wait = self
            .wait_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.stopped.store(true, Ordering::Release);
        self.wake.notify_all();
        if let Ok(active) = self.active.lock() {
            for stream in active.values() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    }

    pub(crate) fn wait(&self, timeout: Duration) {
        if let Ok(guard) = self.wait_lock.lock() {
            let _ = self
                .wake
                .wait_timeout_while(guard, timeout, |_| !self.stopped());
        }
    }

    fn track(self: &Arc<Self>, stream: &Stream) -> Result<Tracked> {
        let mut active = self.active.lock().map_err(|_| err("SYNC_CONFIG"))?;
        if self.stopped() {
            return Err(err("AGENT_UNAVAILABLE"));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        active.insert(
            id,
            stream.try_clone().map_err(|_| err("AGENT_UNAVAILABLE"))?,
        );
        Ok(Tracked {
            control: self.clone(),
            id,
        })
    }
}

struct Tracked {
    control: Arc<Control>,
    id: u64,
}
impl Drop for Tracked {
    fn drop(&mut self) {
        if let Ok(mut active) = self.control.active.lock() {
            active.remove(&self.id);
        }
    }
}

pub(crate) fn connect(path: &Path) -> std::io::Result<Stream> {
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket.connect_timeout(&SockAddr::unix(path)?, Duration::from_secs(1))?;
    #[cfg(unix)]
    {
        use std::os::fd::{FromRawFd, IntoRawFd};
        Ok(unsafe { Stream::from_raw_fd(socket.into_raw_fd()) })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{FromRawSocket, IntoRawSocket};
        Ok(unsafe { Stream::from_raw_socket(socket.into_raw_socket()) })
    }
}

pub(crate) struct SocketGuard {
    pub(crate) path: PathBuf,
    identity: platform::Identity,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if platform::identity(&self.path).ok() == Some(self.identity) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn bind(directory: &Path) -> Result<(Listener, SocketGuard)> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| err("SYNC_CONFIG"))?
        .as_nanos();
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = crate::hash(format!("{nonce}:{}:{sequence}", std::process::id()).as_bytes());
    let name = format!("c-{}.sock", &name[..16]);
    let preferred = directory.join(&name);
    let path = if preferred.to_string_lossy().len() <= 100 {
        preferred
    } else {
        let key = crate::hash(directory.to_string_lossy().as_bytes());
        let temp = std::env::temp_dir();
        let temp = temp.canonicalize().unwrap_or(temp);
        let short = temp.join(format!("acs-{}", &key[..16]));
        platform::private_dir(&short)?;
        short.join(name)
    };
    if path.to_str().is_none()
        || path.to_string_lossy().len() > 100
        || path.to_string_lossy().chars().any(char::is_control)
    {
        return Err(err("SYNC_CONFIG"));
    }
    // A fresh name never needs to remove an existing or potentially active socket.
    let listener = Listener::bind(&path).map_err(|_| err("SYNC_BUSY"))?;
    let guard = SocketGuard {
        identity: platform::identity(&path)?,
        path,
    };
    platform::restrict(&guard.path, false)?;
    listener
        .set_nonblocking(true)
        .map_err(|_| err("SYNC_CONFIG"))?;
    Ok((listener, guard))
}

fn transient(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
    )
}

fn read_some(
    stream: &mut Stream,
    buffer: &mut [u8],
    deadline: Instant,
    control: &Control,
) -> Result<usize> {
    loop {
        if control.stopped() || Instant::now() >= deadline {
            return Err(err("AGENT_UNAVAILABLE"));
        }
        match stream.read(buffer) {
            Ok(count) => return Ok(count),
            Err(error) if transient(&error) => control.wait(IO_TICK),
            Err(_) => return Err(err("AGENT_UNAVAILABLE")),
        }
    }
}

fn write_all(
    stream: &mut Stream,
    mut bytes: &[u8],
    deadline: Instant,
    control: &Control,
) -> Result<()> {
    while !bytes.is_empty() {
        if control.stopped() || Instant::now() >= deadline {
            return Err(err("AGENT_UNAVAILABLE"));
        }
        match stream.write(bytes) {
            Ok(0) => return Err(err("AGENT_UNAVAILABLE")),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if transient(&error) => control.wait(IO_TICK),
            Err(_) => return Err(err("AGENT_UNAVAILABLE")),
        }
    }
    Ok(())
}

fn configure(stream: &Stream) -> Result<()> {
    // Absolute deadlines and cancellable waits also bound slow trickle input.
    // Nonblocking mode avoids reusing a Winsock connection after SO_RCVTIMEO.
    stream
        .set_nonblocking(true)
        .map_err(|_| err("AGENT_UNAVAILABLE"))
}

pub(crate) fn manager_request(
    paths: &ManagerPaths,
    op: &Value,
    control: &Arc<Control>,
) -> Result<Sensitive> {
    if control.stopped() {
        return Err(err("AGENT_UNAVAILABLE"));
    }
    let token = platform::read(&paths.token, 64)
        .map_err(|_| err("AGENT_UNAVAILABLE"))?
        .ok_or_else(|| err("AGENT_UNAVAILABLE"))?;
    if token.len() != 64 || !token.iter().all(u8::is_ascii_hexdigit) {
        return Err(err("AGENT_TOKEN"));
    }
    #[derive(Serialize)]
    struct Request<'a> {
        version: u32,
        token: &'a str,
        op: &'a Value,
    }
    let request = Request {
        version: 1,
        token: std::str::from_utf8(&token).map_err(|_| err("AGENT_TOKEN"))?,
        op,
    };
    let mut bytes =
        Zeroizing::new(serde_json::to_vec(&request).map_err(|_| err("AGENT_PROTOCOL"))?);
    bytes.push(b'\n');
    let mut stream = connect(&paths.socket).map_err(|_| err("AGENT_UNAVAILABLE"))?;
    configure(&stream)?;
    let _tracked = control.track(&stream)?;
    let write_deadline = Instant::now() + REQUEST_TIMEOUT;
    write_all(&mut stream, &bytes, write_deadline, control)?;
    drop(bytes);
    drop(token);
    let deadline = Instant::now()
        + if op["op"] == "sync_run_application" {
            SYNC_TIMEOUT
        } else {
            REQUEST_TIMEOUT
        };
    let mut response = Zeroizing::new(Vec::new());
    let mut chunk = Zeroizing::new([0u8; 4096]);
    loop {
        let count = read_some(&mut stream, &mut *chunk, deadline, control)?;
        if count == 0 {
            return Err(err("AGENT_PROTOCOL"));
        }
        response.extend_from_slice(&chunk[..count]);
        if response.len() > MAX_BODY {
            return Err(err("AGENT_PROTOCOL"));
        }
        if let Some(end) = response.iter().position(|byte| *byte == b'\n') {
            if end + 1 != response.len() {
                return Err(err("AGENT_PROTOCOL"));
            }
            break;
        }
    }
    let mut reply =
        Sensitive(serde_json::from_slice(&response).map_err(|_| err("AGENT_PROTOCOL"))?);
    match reply.0["ok"].as_bool() {
        Some(true) => Ok(Sensitive(reply.0["data"].take())),
        Some(false) => Err(fixed_error(
            reply.0["error"]["code"]
                .as_str()
                .unwrap_or("AGENT_PROTOCOL"),
        )),
        None => Err(err("AGENT_PROTOCOL")),
    }
}

pub(crate) fn serve(listener: Listener, guard: SocketGuard, shared: &Shared) {
    while !shared.control.stopped() {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if configure(&stream).is_err() {
                    continue;
                }
                let Ok(_tracked) = shared.control.track(&stream) else {
                    break;
                };
                let deadline = Instant::now() + CALLBACK_TIMEOUT;
                let response = callback(&mut stream, deadline, shared);
                let (status, body) = match response {
                    Ok(body) => (200, body),
                    Err(code) => {
                        let status = match code.as_str() {
                            "SYNC_LOCAL_CHANGED" => 409,
                            "SYNC_FILE_TOO_LARGE" => 413,
                            "SYNC_CALLBACK" => 400,
                            "NOT_FOUND" => 404,
                            "METHOD_NOT_ALLOWED" => 405,
                            _ => 500,
                        };
                        let body = serde_json::to_vec(&json!({"error_code": code})).unwrap();
                        (status, Zeroizing::new(body))
                    }
                };
                let header = format!(
                    "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = write_all(&mut stream, header.as_bytes(), deadline, &shared.control)
                    .and_then(|_| write_all(&mut stream, &body, deadline, &shared.control));
                let _ = stream.shutdown(Shutdown::Write);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                shared.control.wait(Duration::from_millis(20));
            }
            Err(_) => {
                shared.set_error("SYNC_CALLBACK");
                shared.control.stop();
                break;
            }
        }
    }
    drop(listener);
    drop(guard);
}

fn callback(stream: &mut Stream, deadline: Instant, shared: &Shared) -> Result<Zeroizing<Vec<u8>>> {
    let (method, body) = read_http(stream, deadline, &shared.control)?;
    if shared.control.stopped() {
        return Err(err("AGENT_UNAVAILABLE"));
    }
    match method.as_str() {
        "GET" => {
            if !body.is_empty() {
                return Err(err("SYNC_CALLBACK"));
            }
            let (content, updated_at) = shared
                .state
                .lock()
                .map_err(|_| err("SYNC_CONFIG"))?
                .snapshot(shared.adapter.as_ref())?;
            let encoded = Zeroizing::new(STANDARD.encode(&content));
            #[derive(Serialize)]
            struct Envelope<'a> {
                content: &'a str,
                updated_at: &'a str,
            }
            Ok(Zeroizing::new(
                serde_json::to_vec(&Envelope {
                    content: &encoded,
                    updated_at: &updated_at,
                })
                .map_err(|_| err("SYNC_CONFIG"))?,
            ))
        }
        "PUT" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Envelope {
                #[serde(deserialize_with = "secret_string")]
                content: Zeroizing<String>,
                updated_at: String,
                expected_sha256: String,
            }
            fn secret_string<'de, D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Zeroizing<String>, D::Error> {
                String::deserialize(deserializer).map(Zeroizing::new)
            }
            let envelope: Envelope =
                serde_json::from_slice(&body).map_err(|_| err("SYNC_CONFIG"))?;
            drop(body);
            if envelope.content.len() > MAX_CONTENT.div_ceil(3) * 4 {
                return Err(err("SYNC_FILE_TOO_LARGE"));
            }
            let mut content = Zeroizing::new(Vec::new());
            STANDARD
                .decode_vec(envelope.content.as_bytes(), &mut content)
                .map_err(|_| err("SYNC_CONFIG"))?;
            if content.len() > MAX_CONTENT {
                return Err(err("SYNC_FILE_TOO_LARGE"));
            }
            let value = serde_json::from_slice(&content).map_err(|_| err("SYNC_CONFIG"))?;
            shared.state.lock().map_err(|_| err("SYNC_CONFIG"))?.apply(
                shared.adapter.as_ref(),
                value,
                &envelope.expected_sha256,
                &envelope.updated_at,
            )?;
            Ok(Zeroizing::new(b"{}".to_vec()))
        }
        _ => Err(err("METHOD_NOT_ALLOWED")),
    }
}

fn read_http(
    stream: &mut Stream,
    deadline: Instant,
    control: &Control,
) -> Result<(String, Zeroizing<Vec<u8>>)> {
    let mut received = Zeroizing::new(Vec::with_capacity(MAX_HEADERS));
    let mut chunk = Zeroizing::new([0u8; 4096]);
    let end = loop {
        let remaining = MAX_HEADERS.saturating_sub(received.len());
        if remaining == 0 {
            return Err(err("SYNC_CALLBACK"));
        }
        let limit = chunk.len().min(remaining);
        let count = read_some(stream, &mut chunk[..limit], deadline, control)?;
        if count == 0 {
            return Err(err("SYNC_CALLBACK"));
        }
        received.extend_from_slice(&chunk[..count]);
        if let Some(end) = received.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let text = std::str::from_utf8(&received[..end - 4]).map_err(|_| err("SYNC_CALLBACK"))?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_owned();
    if first.next() != Some("/config") {
        return Err(err("NOT_FOUND"));
    }
    if !matches!(first.next(), Some("HTTP/1.0" | "HTTP/1.1")) || first.next().is_some() {
        return Err(err("SYNC_CALLBACK"));
    }
    let mut length = None;
    let mut seen = HashSet::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or_else(|| err("SYNC_CALLBACK"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || !value.is_ascii()
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\t')
            || !seen.insert(name.to_ascii_lowercase())
            || name.eq_ignore_ascii_case("transfer-encoding")
        {
            return Err(err("SYNC_CALLBACK"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim_matches([' ', '\t']);
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(err("SYNC_CALLBACK"));
            }
            let count = value
                .parse::<usize>()
                .map_err(|_| err("SYNC_FILE_TOO_LARGE"))?;
            if count > MAX_BODY {
                return Err(err("SYNC_FILE_TOO_LARGE"));
            }
            length = Some(count);
        }
    }
    if method == "PUT" && length.is_none() {
        return Err(err("SYNC_CALLBACK"));
    }
    let length = length.unwrap_or(0);
    if received.len() - end > length {
        return Err(err("SYNC_CALLBACK"));
    }
    let mut body = Zeroizing::new(vec![0u8; length]);
    let mut read = received.len() - end;
    body[..read].copy_from_slice(&received[end..]);
    while read < length {
        let count = read_some(stream, &mut body[read..], deadline, control)?;
        if count == 0 {
            return Err(err("SYNC_CALLBACK"));
        }
        read += count;
    }
    Ok((method, body))
}
