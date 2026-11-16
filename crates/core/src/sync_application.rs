//! Credential-bearing callbacks to locally registered applications.

use crate::{err, safe_ancestors, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};
use zeroize::Zeroizing;

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = uds_windows::UnixStream;

const MAX_CONTENT: usize = 1024 * 1024;
const MAX_ENCODED_CONTENT: usize = MAX_CONTENT.div_ceil(3) * 4;
const MAX_HEADERS: usize = 16 * 1024;
const MAX_BODY: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SyncInterface {
    pub socket_path: String,
    pub path: String,
}

pub(crate) fn validate_interface(interface: &SyncInterface) -> Result<()> {
    let socket = Path::new(&interface.socket_path);
    if !socket.is_absolute()
        || interface.socket_path.len() > 100
        || interface.socket_path.chars().any(char::is_control)
        || !interface.path.starts_with('/')
        || interface.path.len() > 2048
        || !interface.path.is_ascii()
        || interface
            .path
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b' ' | b'?' | b'#'))
    {
        return Err(err("SYNC_INTERFACE"));
    }
    let parent = socket.parent().ok_or_else(|| err("SYNC_INTERFACE"))?;
    safe_ancestors(parent).map_err(|_| err("SYNC_INTERFACE"))?;
    match fs::symlink_metadata(socket) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_dir() => {
            Err(err("SYNC_INTERFACE"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(err("SYNC_INTERFACE")),
    }
}

pub(crate) struct ApplicationConfig {
    pub content: Zeroizing<Vec<u8>>,
    pub updated_at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallbackConfig {
    #[serde(deserialize_with = "deserialize_content")]
    content: Zeroizing<String>,
    updated_at: String,
}

// Wrapping the field during deserialization also wipes it when a later field
// or trailing JSON is invalid, before CallbackConfig can be constructed.
fn deserialize_content<'de, D>(deserializer: D) -> std::result::Result<Zeroizing<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Zeroizing::new)
}

pub(crate) fn fetch(interface: &SyncInterface) -> Result<ApplicationConfig> {
    let response = request(interface, "GET", &[])?;
    if response.status != 200 {
        return Err(err("SYNC_CALLBACK"));
    }
    let config: CallbackConfig =
        serde_json::from_slice(&response.body).map_err(|_| err("SYNC_CALLBACK"))?;
    if chrono::DateTime::parse_from_rfc3339(&config.updated_at).is_err() {
        return Err(err("SYNC_CALLBACK"));
    }
    if config.content.len() > MAX_ENCODED_CONTENT {
        return Err(err("SYNC_FILE_TOO_LARGE"));
    }
    let mut content = Zeroizing::new(Vec::with_capacity(config.content.len().div_ceil(4) * 3));
    STANDARD
        .decode_vec(config.content.as_bytes(), &mut content)
        .map_err(|_| err("SYNC_CALLBACK"))?;
    if content.len() > MAX_CONTENT {
        return Err(err("SYNC_FILE_TOO_LARGE"));
    }
    Ok(ApplicationConfig {
        content,
        updated_at: config.updated_at,
    })
}

pub(crate) fn apply(
    interface: &SyncInterface,
    content: &[u8],
    updated_at: &str,
    expected_sha256: &str,
) -> Result<()> {
    if content.len() > MAX_CONTENT {
        return Err(err("SYNC_FILE_TOO_LARGE"));
    }
    if updated_at.len() > MAX_BODY
        || chrono::DateTime::parse_from_rfc3339(updated_at).is_err()
        || expected_sha256.len() != 64
        || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(err("SYNC_CALLBACK"));
    }
    #[derive(Serialize)]
    struct ApplyRequest<'a> {
        content: &'a str,
        updated_at: &'a str,
        expected_sha256: &'a str,
    }
    let encoded = Zeroizing::new(STANDARD.encode(content));
    let capacity = encoded.len() + updated_at.len() + expected_sha256.len() + 128;
    if capacity > MAX_BODY {
        return Err(err("SYNC_FILE_TOO_LARGE"));
    }
    let mut body = Zeroizing::new(Vec::with_capacity(capacity));
    serde_json::to_writer(
        &mut *body,
        &ApplyRequest {
            content: &encoded,
            updated_at,
            expected_sha256,
        },
    )
    .map_err(|_| err("SYNC_CALLBACK"))?;
    match request(interface, "PUT", &body)?.status {
        200 | 204 => Ok(()),
        409 => Err(err("SYNC_LOCAL_CHANGED")),
        _ => Err(err("SYNC_CALLBACK")),
    }
}

struct Response {
    status: u16,
    body: Zeroizing<Vec<u8>>,
}

fn offline_error(error: std::io::Error) -> crate::Error {
    match error.kind() {
        std::io::ErrorKind::NotFound
        | std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::NotConnected
        | std::io::ErrorKind::TimedOut
        | std::io::ErrorKind::WouldBlock => err("SYNC_APPLICATION_OFFLINE"),
        _ => err("SYNC_CALLBACK"),
    }
}

fn request(interface: &SyncInterface, method: &str, body: &[u8]) -> Result<Response> {
    validate_interface(interface)?;
    // This connection is independent of the manager client: only these fixed
    // headers and the callback payload are sent, with no authentication token.
    let headers = format!(
        "{method} {} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        interface.path,
        body.len()
    );
    if headers.len() > MAX_HEADERS || body.len() > MAX_BODY {
        return Err(err("SYNC_CALLBACK"));
    }
    let mut stream = Stream::connect(&interface.socket_path).map_err(offline_error)?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .and_then(|_| stream.set_write_timeout(Some(TIMEOUT)))
        .map_err(|_| err("SYNC_CALLBACK"))?;
    stream
        .write_all(headers.as_bytes())
        .and_then(|_| stream.write_all(body))
        .and_then(|_| stream.flush())
        .map_err(offline_error)?;
    read_response(stream)
}

fn read_response(mut stream: impl Read) -> Result<Response> {
    // No BufReader: its internal allocation could retain credential bytes.
    let mut received = Zeroizing::new(Vec::with_capacity(MAX_HEADERS));
    let mut chunk = Zeroizing::new([0u8; 4096]);
    let header_end = loop {
        let remaining = MAX_HEADERS - received.len();
        if remaining == 0 {
            return Err(err("SYNC_CALLBACK"));
        }
        let limit = remaining.min(chunk.len());
        let count = stream.read(&mut chunk[..limit]).map_err(offline_error)?;
        if count == 0 {
            return Err(err("SYNC_CALLBACK"));
        }
        received.extend_from_slice(&chunk[..count]);
        if let Some(end) = received.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let (status, length) = parse_headers(&received[..header_end])?;
    let mut body = Zeroizing::new(Vec::with_capacity(length.unwrap_or(MAX_BODY + 1)));
    body.extend_from_slice(&received[header_end..]);
    if status == 204 {
        if length.unwrap_or(0) != 0 || !body.is_empty() {
            return Err(err("SYNC_CALLBACK"));
        }
    } else if let Some(length) = length {
        if body.len() > length {
            return Err(err("SYNC_CALLBACK"));
        }
        let buffered = body.len();
        body.resize(length, 0);
        stream
            .read_exact(&mut body[buffered..])
            .map_err(offline_error)?;
    } else {
        stream
            .take((MAX_BODY + 1 - body.len()) as u64)
            .read_to_end(&mut body)
            .map_err(offline_error)?;
        if body.len() > MAX_BODY {
            return Err(err("SYNC_FILE_TOO_LARGE"));
        }
    }
    Ok(Response { status, body })
}

fn parse_headers(headers: &[u8]) -> Result<(u16, Option<usize>)> {
    let text = std::str::from_utf8(headers)
        .ok()
        .and_then(|text| text.strip_suffix("\r\n\r\n"))
        .ok_or_else(|| err("SYNC_CALLBACK"))?;
    let mut lines = text.split("\r\n");
    let (version, status) = lines
        .next()
        .and_then(|line| line.split_once(' '))
        .ok_or_else(|| err("SYNC_CALLBACK"))?;
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || !status.is_ascii()
        || status.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(err("SYNC_CALLBACK"));
    }
    let status = status.split(' ').next().unwrap_or("");
    if status.len() != 3 || !status.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(err("SYNC_CALLBACK"));
    }
    let status = status.parse::<u16>().map_err(|_| err("SYNC_CALLBACK"))?;
    if !(100..600).contains(&status) {
        return Err(err("SYNC_CALLBACK"));
    }
    let mut length = None;
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
            || name.eq_ignore_ascii_case("transfer-encoding")
        {
            return Err(err("SYNC_CALLBACK"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim_matches([' ', '\t']);
            if length.is_some()
                || value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(err("SYNC_CALLBACK"));
            }
            let count = value.parse::<usize>().map_err(|_| err("SYNC_CALLBACK"))?;
            if count > MAX_BODY {
                return Err(err("SYNC_FILE_TOO_LARGE"));
            }
            length = Some(count);
        }
    }
    Ok((status, length))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::BufRead, io::BufReader, thread};

    #[cfg(unix)]
    type Listener = std::os::unix::net::UnixListener;
    #[cfg(windows)]
    type Listener = uds_windows::UnixListener;

    const TIMESTAMP: &str = "2026-11-12T08:09:10.123+02:00";

    fn interface() -> (tempfile::TempDir, SyncInterface) {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().canonicalize().unwrap().join("sync.sock");
        (
            directory,
            SyncInterface {
                socket_path: socket.to_str().unwrap().into(),
                path: "/config".into(),
            },
        )
    }

    fn read_request(stream: &mut Stream) -> (String, Zeroizing<Vec<u8>>) {
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        let mut reader = BufReader::new(stream);
        let mut headers = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            headers.push_str(&line);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
        assert!(!headers.to_ascii_lowercase().contains("x-agent-token:"));
        let mut body = Zeroizing::new(vec![0; length]);
        reader.read_exact(&mut body).unwrap();
        (headers, body)
    }

    fn write_response(stream: &mut Stream, status: u16, body: &[u8]) {
        write!(
            stream,
            "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }

    #[test]
    fn callback_get_and_put_roundtrip_preserves_bytes_timestamp_and_digest() {
        let (_directory, interface) = interface();
        let listener = Listener::bind(&interface.socket_path).unwrap();
        let original = b"synthetic-api-key\0\xff\n";
        let replacement = b"new-synthetic-api-key\0\x80";
        let expected = crate::hash(original);
        let server_expected = expected.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (headers, body) = read_request(&mut stream);
            assert!(headers.starts_with("GET /config HTTP/1.1\r\n"));
            assert!(body.is_empty());
            let reply = serde_json::to_vec(&serde_json::json!({
                "content": STANDARD.encode(original), "updated_at": TIMESTAMP,
            }))
            .unwrap();
            write_response(&mut stream, 200, &reply);
            drop(stream);

            let (mut stream, _) = listener.accept().unwrap();
            let (headers, body) = read_request(&mut stream);
            assert!(headers.starts_with("PUT /config HTTP/1.1\r\n"));
            let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(payload["content"], STANDARD.encode(replacement));
            assert_eq!(payload["updated_at"], TIMESTAMP);
            assert_eq!(payload["expected_sha256"], server_expected);
            assert_eq!(payload.as_object().unwrap().len(), 3);
            write_response(&mut stream, 204, &[]);
        });
        let config = fetch(&interface).unwrap();
        assert_eq!(&*config.content, original);
        assert_eq!(config.updated_at, TIMESTAMP);
        apply(&interface, replacement, TIMESTAMP, &expected).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn unauthorized_and_conflicting_callbacks_return_fixed_errors() {
        for status in [401, 409, 302] {
            let (_directory, interface) = interface();
            let listener = Listener::bind(&interface.socket_path).unwrap();
            let server = thread::spawn(move || {
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().unwrap();
                    read_request(&mut stream);
                    write_response(&mut stream, status, b"private-response-do-not-echo");
                }
            });
            let fetch_error = fetch(&interface).err().unwrap();
            assert_eq!(fetch_error, err("SYNC_CALLBACK"));
            let apply_error = apply(&interface, b"synthetic", TIMESTAMP, &crate::hash(b"old"))
                .err()
                .unwrap();
            assert_eq!(
                apply_error,
                err(if status == 409 {
                    "SYNC_LOCAL_CHANGED"
                } else {
                    "SYNC_CALLBACK"
                })
            );
            assert!(!apply_error.to_string().contains("private-response"));
            server.join().unwrap();
        }
    }

    #[test]
    fn response_framing_rejects_oversized_ambiguous_and_truncated_data() {
        let cases = [
            (
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    MAX_BODY + 1
                ),
                "SYNC_FILE_TOO_LARGE",
            ),
            (
                format!(
                    "HTTP/1.1 200 OK\r\nX-Long: {}\r\n\r\n",
                    "x".repeat(MAX_HEADERS)
                ),
                "SYNC_CALLBACK",
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nshort".into(),
                "SYNC_CALLBACK",
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\ncontent-length: 0\r\n\r\n".into(),
                "SYNC_CALLBACK",
            ),
            (
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".into(),
                "SYNC_CALLBACK",
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 0".into(),
                "SYNC_CALLBACK",
            ),
            (
                "HTTP/1.1 204 No Content\r\nContent-Length: 1\r\n\r\nx".into(),
                "SYNC_CALLBACK",
            ),
            (
                format!("HTTP/1.0 200 OK\r\n\r\n{}", "x".repeat(MAX_BODY + 1)),
                "SYNC_FILE_TOO_LARGE",
            ),
        ];
        for (response, code) in cases {
            let error = read_response(std::io::Cursor::new(response.as_bytes()))
                .err()
                .unwrap();
            assert_eq!(error, err(code));
        }
    }

    #[test]
    fn fetch_rejects_bad_json_base64_timestamps_and_decoded_size() {
        let (_directory, interface) = interface();
        let listener = Listener::bind(&interface.socket_path).unwrap();
        let replies = [
            br#"{"content":"c2VjcmV0","updated_at":"not-a-time"}"#.to_vec(),
            format!(r#"{{"content":"?","updated_at":"{TIMESTAMP}"}}"#).into_bytes(),
            br#"{"content":"c2VjcmV0"}"#.to_vec(),
            serde_json::to_vec(&serde_json::json!({
                "content": STANDARD.encode(vec![0; MAX_CONTENT + 1]),
                "updated_at": TIMESTAMP,
            }))
            .unwrap(),
        ];
        let server = thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                read_request(&mut stream);
                write_response(&mut stream, 200, &reply);
            }
        });
        for code in [
            "SYNC_CALLBACK",
            "SYNC_CALLBACK",
            "SYNC_CALLBACK",
            "SYNC_FILE_TOO_LARGE",
        ] {
            assert_eq!(fetch(&interface).err().unwrap(), err(code));
        }
        server.join().unwrap();
    }

    #[test]
    fn registration_accepts_missing_socket_and_rejects_invalid_paths() {
        let (_directory, interface) = interface();
        validate_interface(&interface).unwrap();
        for path in ["", "config", "/has space", "/a\r\nb", "/a?x", "/a#x", "/é"] {
            let mut invalid = interface.clone();
            invalid.path = path.into();
            assert_eq!(validate_interface(&invalid), Err(err("SYNC_INTERFACE")));
        }
        let mut invalid = interface.clone();
        invalid.path = format!("/{}", "x".repeat(2048));
        assert_eq!(validate_interface(&invalid), Err(err("SYNC_INTERFACE")));
        for path in [
            "relative.sock".into(),
            format!("{}\n", interface.socket_path),
            format!("{}{}", interface.socket_path, "x".repeat(101)),
        ] {
            invalid = interface.clone();
            invalid.socket_path = path;
            assert_eq!(validate_interface(&invalid), Err(err("SYNC_INTERFACE")));
        }
    }

    #[test]
    fn offline_applications_report_a_distinct_code() {
        let (_directory, interface) = interface();
        assert_eq!(
            fetch(&interface).err().unwrap().code,
            "SYNC_APPLICATION_OFFLINE"
        );
        assert_eq!(
            apply(&interface, b"synthetic", TIMESTAMP, &crate::hash(b"old"))
                .err()
                .unwrap()
                .code,
            "SYNC_APPLICATION_OFFLINE"
        );
    }

    #[cfg(unix)]
    #[test]
    fn registration_rejects_symlinked_socket_or_ancestor() {
        let (directory, interface) = interface();
        let root = directory.path().canonicalize().unwrap();
        let real = root.join("real");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, root.join("link")).unwrap();
        let mut invalid = interface.clone();
        invalid.socket_path = root.join("link/sync.sock").to_str().unwrap().into();
        assert_eq!(validate_interface(&invalid), Err(err("SYNC_INTERFACE")));
        std::os::unix::fs::symlink(real.join("missing.sock"), &interface.socket_path).unwrap();
        assert_eq!(validate_interface(&interface), Err(err("SYNC_INTERFACE")));
    }
}
