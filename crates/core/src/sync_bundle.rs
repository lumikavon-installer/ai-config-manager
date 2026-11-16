use super::*;
use chrono::{DateTime, Utc};

#[derive(Clone, Serialize, Deserialize)]
struct BundleItem {
    id: String,
    label: String,
    kind: SyncKind,
    path: Option<String>,
    #[serde(default)]
    updated_at: String,
    #[serde(deserialize_with = "read_content", serialize_with = "write_content")]
    content: Zeroizing<String>,
}
fn read_content<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Zeroizing<String>, D::Error> {
    String::deserialize(input).map(Zeroizing::new)
}
fn write_content<S: serde::Serializer>(
    value: &Zeroizing<String>,
    output: S,
) -> std::result::Result<S::Ok, S::Error> {
    output.serialize_str(value)
}
#[derive(Serialize, Deserialize)]
struct Bundle {
    format: String,
    created_at: String,
    items: Vec<BundleItem>,
}
struct LocalSnapshot {
    item: SyncItem,
    data: Option<BundleItem>,
    revision: String,
}
pub(super) struct SyncResult {
    pub remote: RemoteFile,
    pub item_count: usize,
    pub uploaded_count: usize,
    pub downloaded_count: usize,
    pub conflict_count: usize,
    /// Items whose local side could not be read or updated. Their remote copy is
    /// kept and the next attempt retries them.
    pub deferred: Vec<DeferredItem>,
}

impl SyncStore {
    pub(super) fn synchronize_target(
        &self,
        provider: SyncProvider,
        repo: &str,
        git_branch: &str,
        token: &str,
        secret: &str,
        items: &[SyncItem],
    ) -> Result<SyncResult> {
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("ai-config-manager/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| err("GITHUB_NETWORK"))?;
        let url = format!("{}/repos/{repo}/contents/{SYNC_REMOTE_PATH}", self.api_base);
        let git = if provider == SyncProvider::Git {
            Some(git::GitRepository::new(repo, git_branch, None)?)
        } else {
            None
        };
        for attempt in 0..3 {
            let (local, mut deferred) = self.capture_local(items)?;
            let (mut remote, previous) = if let Some(git) = &git {
                match git.fetch()? {
                    Some(snapshot) => (
                        RemoteFile {
                            sha: Some(snapshot.head),
                            url: None,
                        },
                        match snapshot.encrypted {
                            Some(bytes) => decode_bundle(secret, &bytes)?,
                            None => BTreeMap::new(),
                        },
                    ),
                    None => (RemoteFile::default(), BTreeMap::new()),
                }
            } else {
                self.fetch_remote(&client, &url, repo, token, secret)?
            };
            let mut merged = previous.clone();
            let mut downloads = Vec::new();
            let mut uploaded = 0;
            let mut conflicts = 0;
            for snapshot in &local {
                let old = previous.get(&snapshot.item.id);
                if old.is_some_and(|old| old.kind != snapshot.item.kind) {
                    return Err(err("SYNC_BUNDLE"));
                }
                match (snapshot.data.as_ref(), old) {
                    (Some(new), Some(old)) => {
                        let differs = new.content != old.content;
                        if differs {
                            conflicts += 1;
                        }
                        let local_wins =
                            (timestamp(&new.updated_at)?, hash(new.content.as_bytes()))
                                >= (timestamp(&old.updated_at)?, hash(old.content.as_bytes()));
                        if local_wins {
                            if differs {
                                uploaded += 1;
                            }
                            merged.insert(new.id.clone(), new.clone());
                        } else if differs {
                            downloads.push((snapshot, old));
                        }
                    }
                    (Some(new), None) => {
                        uploaded += 1;
                        merged.insert(new.id.clone(), new.clone());
                    }
                    (None, Some(old)) => downloads.push((snapshot, old)),
                    (None, None) => {}
                }
            }
            if merged.is_empty() {
                if deferred.is_empty() {
                    return Err(err("SYNC_NOT_CONFIGURED"));
                }
                // Every enabled item is waiting for its application; the remote
                // bundle is unchanged and must not be replaced with an empty one.
                return Ok(SyncResult {
                    remote,
                    item_count: deferred.len(),
                    uploaded_count: 0,
                    downloaded_count: 0,
                    conflict_count: conflicts,
                    deferred,
                });
            }
            self.record_progress(local.len(), 0, 0, conflicts, remote.sha.clone());
            let unchanged = merged.len() == previous.len()
                && merged.iter().all(|(id, item)| {
                    previous.get(id).is_some_and(|old| {
                        old.content == item.content && old.updated_at == item.updated_at
                    })
                });
            if !unchanged || remote.sha.is_none() {
                let bundle = Bundle {
                    format: "ai-config-manager-sync-bundle-v1".into(),
                    created_at: Utc::now().to_rfc3339(),
                    items: merged.into_values().collect(),
                };
                let plaintext =
                    Zeroizing::new(serde_json::to_vec(&bundle).map_err(|_| err("SYNC_BUNDLE"))?);
                if plaintext.len() as u64 > MAX_BUNDLE_BYTES {
                    return Err(err("SYNC_FILE_TOO_LARGE"));
                }
                let encrypted = encrypt_bundle(secret, &plaintext)?;
                let pushed = if let Some(git) = &git {
                    git.push(&encrypted, remote.sha.as_deref())
                        .map(|sha| RemoteFile {
                            sha: Some(sha),
                            url: None,
                        })
                } else {
                    push_remote(&client, &url, token, &encrypted, remote.sha.as_deref())
                };
                match pushed {
                    Ok(updated) => remote = updated,
                    Err(error)
                        if matches!(
                            error.code.as_str(),
                            "GITHUB_CONFLICT" | "GITHUB_REJECTED" | "GIT_CONFLICT"
                        ) && attempt < 2 =>
                    {
                        continue
                    }
                    Err(error) => return Err(error),
                }
            }
            self.record_progress(local.len(), uploaded, 0, conflicts, remote.sha.clone());
            let mut applied = 0;
            for (snapshot, selected) in &downloads {
                match self.apply_remote(snapshot, selected) {
                    Ok(()) => {
                        applied += 1;
                        self.record_progress(
                            local.len(),
                            uploaded,
                            applied,
                            conflicts,
                            remote.sha.clone(),
                        );
                    }
                    Err(error) if snapshot.item.kind == SyncKind::Application => {
                        deferred.push(DeferredItem {
                            id: snapshot.item.id.clone(),
                            label: snapshot.item.label.clone(),
                            kind: snapshot.item.kind.clone(),
                            reason: error.code,
                        });
                    }
                    Err(error) => return Err(error),
                }
            }
            return Ok(SyncResult {
                remote,
                item_count: local
                    .iter()
                    .filter(|snapshot| {
                        snapshot.data.is_some() || previous.contains_key(&snapshot.item.id)
                    })
                    .count(),
                uploaded_count: uploaded,
                downloaded_count: applied,
                conflict_count: conflicts,
                deferred,
            });
        }
        Err(err(if provider == SyncProvider::Git {
            "GIT_CONFLICT"
        } else {
            "GITHUB_CONFLICT"
        }))
    }

    fn record_progress(
        &self,
        count: usize,
        uploaded: usize,
        downloaded: usize,
        conflicts: usize,
        sha: Option<String>,
    ) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(record) = state.active_record.as_mut() {
                record.item_count = count;
                record.uploaded_count = uploaded;
                record.downloaded_count = downloaded;
                record.conflict_count = conflicts;
                record.remote_sha = sha;
            }
        }
    }

    fn capture_local(&self, items: &[SyncItem]) -> Result<(Vec<LocalSnapshot>, Vec<DeferredItem>)> {
        let mut snapshots = Vec::with_capacity(items.len());
        let mut deferred = Vec::new();
        let mut total = 0;
        for item in items {
            let (content, revision, updated_at) = match item.kind {
                SyncKind::AiConfig => match self.store.sync_export()? {
                    Some((yaml, revision, time)) => {
                        (Zeroizing::new(yaml.as_bytes().to_vec()), revision, time)
                    }
                    None => {
                        snapshots.push(LocalSnapshot {
                            item: item.clone(),
                            data: None,
                            revision: "missing".into(),
                        });
                        continue;
                    }
                },
                SyncKind::File => {
                    let path =
                        validate_file_path(item.path.as_deref().ok_or_else(|| err("SYNC_PATH"))?)?;
                    let before = fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .map_err(|_| err("IO"))?;
                    let content =
                        Zeroizing::new(read_bounded(&path)?.ok_or_else(|| err("SYNC_PATH"))?);
                    let after = fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .map_err(|_| err("IO"))?;
                    if before != after {
                        return Err(err("SYNC_LOCAL_CHANGED"));
                    }
                    let revision = hash(&content);
                    (content, revision, DateTime::<Utc>::from(after).to_rfc3339())
                }
                SyncKind::Application => {
                    let interface = &item
                        .application
                        .as_ref()
                        .ok_or_else(|| err("SYNC_APPLICATION"))?
                        .sync_interface;
                    match crate::sync_application::fetch(interface) {
                        Ok(config) => {
                            let revision = hash(&config.content);
                            (config.content, revision, config.updated_at)
                        }
                        // A third-party application that is not running, or that
                        // answers badly, must not block the other items.
                        Err(error) => {
                            deferred.push(DeferredItem {
                                id: item.id.clone(),
                                label: item.label.clone(),
                                kind: item.kind.clone(),
                                reason: error.code,
                            });
                            continue;
                        }
                    }
                }
            };
            if content.len() as u64 > MAX_ITEM_BYTES {
                return Err(err("SYNC_FILE_TOO_LARGE"));
            }
            total += (content.len() as u64).div_ceil(3) * 4;
            if total > MAX_BUNDLE_BYTES {
                return Err(err("SYNC_FILE_TOO_LARGE"));
            }
            snapshots.push(LocalSnapshot {
                item: item.clone(),
                revision,
                data: Some(BundleItem {
                    id: item.id.clone(),
                    label: item.label.clone(),
                    kind: item.kind.clone(),
                    path: item.path.clone(),
                    updated_at,
                    content: Zeroizing::new(STANDARD.encode(&content)),
                }),
            });
        }
        Ok((snapshots, deferred))
    }

    fn apply_remote(&self, local: &LocalSnapshot, selected: &BundleItem) -> Result<()> {
        let content = decode_content(&selected.content)?;
        match local.item.kind {
            SyncKind::AiConfig => {
                self.store
                    .sync_import(&content, &local.revision, &selected.updated_at)
            }
            SyncKind::File => {
                let path = validate_file_path(
                    local.item.path.as_deref().ok_or_else(|| err("SYNC_PATH"))?,
                )?;
                let current =
                    Zeroizing::new(read_bounded(&path)?.ok_or_else(|| err("SYNC_LOCAL_CHANGED"))?);
                if hash(&current) != local.revision {
                    return Err(err("SYNC_LOCAL_CHANGED"));
                }
                write_atomic(&path, &content)?;
                let modified: std::time::SystemTime = timestamp(&selected.updated_at)?.into();
                set_modified(&path, modified)
            }
            SyncKind::Application => crate::sync_application::apply(
                &local
                    .item
                    .application
                    .as_ref()
                    .ok_or_else(|| err("SYNC_APPLICATION"))?
                    .sync_interface,
                &content,
                &selected.updated_at,
                &local.revision,
            ),
        }
    }

    fn fetch_remote(
        &self,
        client: &reqwest::blocking::Client,
        url: &str,
        repo: &str,
        token: &str,
        secret: &str,
    ) -> Result<(RemoteFile, BTreeMap<String, BundleItem>)> {
        let response = github_request(client.get(url), token)
            .send()
            .map_err(map_github_error)?;
        if response.status().as_u16() == 404 {
            return Ok((RemoteFile::default(), BTreeMap::new()));
        }
        if !response.status().is_success() {
            return Err(github_status_error(response.status().as_u16()));
        }
        let mut metadata = read_json(response)?;
        let sha = metadata
            .get("sha")
            .and_then(JsonValue::as_str)
            .filter(|sha| !sha.is_empty())
            .ok_or_else(|| err("GITHUB_REJECTED"))?
            .to_string();
        let remote = RemoteFile {
            sha: Some(sha.clone()),
            url: metadata
                .get("html_url")
                .and_then(JsonValue::as_str)
                .map(str::to_string),
        };
        if metadata
            .get("content")
            .and_then(JsonValue::as_str)
            .is_none_or(str::is_empty)
        {
            if sha.len() > 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(err("GITHUB_REJECTED"));
            }
            let response = github_request(
                client.get(format!("{}/repos/{repo}/git/blobs/{sha}", self.api_base)),
                token,
            )
            .send()
            .map_err(map_github_error)?;
            if !response.status().is_success() {
                return Err(github_status_error(response.status().as_u16()));
            }
            metadata = read_json(response)?;
        }
        let encoded = metadata
            .get("content")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| err("GITHUB_REJECTED"))?;
        let envelope = STANDARD
            .decode(encoded.split_whitespace().collect::<String>())
            .map_err(|_| err("SYNC_DECRYPT"))?;
        Ok((remote, decode_bundle(secret, &envelope)?))
    }
}

fn decode_bundle(secret: &str, envelope: &[u8]) -> Result<BTreeMap<String, BundleItem>> {
    let plaintext = decrypt_bundle(secret, envelope)?;
    let bundle: Bundle = serde_json::from_slice(&plaintext).map_err(|_| err("SYNC_BUNDLE"))?;
    if bundle.format != "ai-config-manager-sync-bundle-v1" {
        return Err(err("SYNC_BUNDLE"));
    }
    timestamp(&bundle.created_at)?;
    let mut items = BTreeMap::new();
    for mut item in bundle.items {
        if item.id.is_empty() || item.id.len() > 256 || !valid_label(&item.label) {
            return Err(err("SYNC_BUNDLE"));
        }
        if item.updated_at.is_empty() {
            item.updated_at = bundle.created_at.clone();
        }
        timestamp(&item.updated_at)?;
        decode_content(&item.content)?;
        if items.insert(item.id.clone(), item).is_some() {
            return Err(err("SYNC_BUNDLE"));
        }
    }
    Ok(items)
}

pub(super) fn github_request(
    request: reqwest::blocking::RequestBuilder,
    token: &str,
) -> reqwest::blocking::RequestBuilder {
    request
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
}
pub(super) fn read_json(response: reqwest::blocking::Response) -> Result<JsonValue> {
    let mut bytes = Zeroizing::new(Vec::new());
    response
        .take(MAX_BUNDLE_BYTES * 3 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| err("GITHUB_NETWORK"))?;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES * 3 {
        return Err(err("SYNC_FILE_TOO_LARGE"));
    }
    serde_json::from_slice(&bytes).map_err(|_| err("GITHUB_REJECTED"))
}
fn push_remote(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &str,
    encrypted: &str,
    sha: Option<&str>,
) -> Result<RemoteFile> {
    let mut body = json!({ "message": "Sync AI Config Manager snapshot", "content": STANDARD.encode(encrypted.as_bytes()) });
    if let Some(sha) = sha {
        body["sha"] = sha.into();
    }
    let response = github_request(client.put(url), token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(&body).map_err(|_| err("SYNC_BUNDLE"))?)
        .send()
        .map_err(map_github_error)?;
    if !response.status().is_success() {
        return Err(github_status_error(response.status().as_u16()));
    }
    let value = read_json(response)?;
    let content = value.get("content").ok_or_else(|| err("GITHUB_REJECTED"))?;
    Ok(RemoteFile {
        sha: Some(
            content
                .get("sha")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| err("GITHUB_REJECTED"))?
                .into(),
        ),
        url: content
            .get("html_url")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
    })
}
fn timestamp(value: &str) -> Result<DateTime<chrono::FixedOffset>> {
    if value.len() > 64 {
        return Err(err("SYNC_BUNDLE"));
    }
    DateTime::parse_from_rfc3339(value).map_err(|_| err("SYNC_BUNDLE"))
}
fn decode_content(value: &str) -> Result<Zeroizing<Vec<u8>>> {
    if value.len() as u64 > MAX_ITEM_BYTES.div_ceil(3) * 4 {
        return Err(err("SYNC_FILE_TOO_LARGE"));
    }
    let decoded = Zeroizing::new(STANDARD.decode(value).map_err(|_| err("SYNC_BUNDLE"))?);
    if decoded.len() as u64 > MAX_ITEM_BYTES {
        return Err(err("SYNC_FILE_TOO_LARGE"));
    }
    Ok(decoded)
}
fn decrypt_bundle(secret: &str, encoded: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let envelope: SyncEnvelope =
        serde_json::from_slice(encoded).map_err(|_| err("SYNC_DECRYPT"))?;
    if envelope.format != "ai-config-manager-sync-v1" || envelope.kdf != "argon2id" {
        return Err(err("SYNC_DECRYPT"));
    }
    let salt = STANDARD
        .decode(envelope.salt)
        .map_err(|_| err("SYNC_DECRYPT"))?;
    let nonce = STANDARD
        .decode(envelope.nonce)
        .map_err(|_| err("SYNC_DECRYPT"))?;
    let ciphertext = STANDARD
        .decode(envelope.ciphertext)
        .map_err(|_| err("SYNC_DECRYPT"))?;
    if salt.len() != 16 || nonce.len() != 12 {
        return Err(err("SYNC_DECRYPT"));
    }
    let key = Zeroizing::new(derive_key(secret, &salt)?);
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| err("SYNC_DECRYPT"))?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: SYNC_AAD,
                },
            )
            .map_err(|_| err("SYNC_DECRYPT"))?,
    );
    if plaintext.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(err("SYNC_FILE_TOO_LARGE"));
    }
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashMap,
        io::{BufRead, BufReader},
        net::{TcpListener, TcpStream},
        thread,
    };

    #[derive(Default)]
    struct Secrets(Mutex<HashMap<String, String>>);
    impl SyncSecrets for Secrets {
        fn get(&self, account: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }
        fn set(&self, account: &str, value: &str) -> Result<()> {
            self.0.lock().unwrap().insert(account.into(), value.into());
            Ok(())
        }
    }
    struct NoVault;
    impl Vault for NoVault {
        fn get_key(&self) -> Result<Option<SecretKey>> {
            panic!("no native credentials in merge tests")
        }
        fn create_key(&self) -> Result<SecretKey> {
            panic!("no native credentials in merge tests")
        }
    }
    fn setup() -> (
        tempfile::TempDir,
        Arc<SyncStore>,
        PathBuf,
        String,
        TcpListener,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let store = Arc::new(Store::new(root.join("ai.yml"), Arc::new(NoVault)));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let sync = Arc::new(
            SyncStore::new(root.join("sync.json"), store, Arc::new(Secrets::default()))
                .with_api_base(format!("http://{}", listener.local_addr().unwrap())),
        );
        sync.save_settings(
            "example/configs",
            Some("test-token".into()),
            Some("test-sync-secret".into()),
        )
        .unwrap();
        sync.set_item_enabled(AI_CONFIG_ITEM_ID, false).unwrap();
        let file = root.join("app.json");
        fs::write(&file, b"local").unwrap();
        set_modified(&file, timestamp("2026-01-01T00:00:00Z").unwrap().into()).unwrap();
        let view = sync
            .add_item(Some("App settings".into()), file.to_str().unwrap(), true)
            .unwrap();
        let id = view.items[1].id.clone();
        (directory, sync, file, id, listener)
    }
    fn encrypted_item(id: &str, content: &str, time: &str) -> String {
        let bundle = json!({ "format":"ai-config-manager-sync-bundle-v1", "created_at": time, "items":[
            { "id": id, "label":"App settings", "kind":"file", "path":"/never/use/remote/path", "content": STANDARD.encode(content), "updated_at":time },
            { "id":"only-on-another-device", "label":"Other app", "kind":"file", "path":null, "content": STANDARD.encode("keep me"), "updated_at":time }
        ] });
        STANDARD.encode(
            encrypt_bundle("test-sync-secret", &serde_json::to_vec(&bundle).unwrap()).unwrap(),
        )
    }
    fn read_request(listener: &TcpListener) -> (TcpStream, String) {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("test server did not receive request: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut text = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            text.push_str(&line);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        text.push_str(&String::from_utf8(body).unwrap());
        (stream, text)
    }
    fn respond(mut stream: TcpStream, code: u16, body: &str) {
        write!(
            stream,
            "HTTP/1.1 {code} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }

    #[test]
    fn remote_newer_wins_locally_and_remote_paths_are_never_used() {
        let (_dir, sync, file, id, listener) = setup();
        let encrypted = encrypted_item(&id, "remote newer", "2026-01-02T00:00:00Z");
        let server = thread::spawn(move || {
            let (stream, request) = read_request(&listener);
            assert!(request.starts_with("GET "));
            respond(
                stream,
                200,
                &json!({"sha":"remote-sha", "content":encrypted}).to_string(),
            );
        });
        let view = sync.run_sync().unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"remote newer");
        assert_eq!(
            DateTime::<Utc>::from(fs::metadata(file).unwrap().modified().unwrap()).to_rfc3339(),
            "2026-01-02T00:00:00+00:00"
        );
        assert_eq!(view.records[0].downloaded_count, 1);
        assert_eq!(view.records[0].uploaded_count, 0);
        assert_eq!(view.records[0].conflict_count, 1);
    }

    #[test]
    fn github_conflict_refetches_and_preserves_an_even_newer_remote_version() {
        let (_dir, sync, file, id, listener) = setup();
        let old = encrypted_item(&id, "remote old", "2025-12-01T00:00:00Z");
        let new = encrypted_item(&id, "remote won race", "2026-01-03T00:00:00Z");
        let server = thread::spawn(move || {
            let (stream, _) = read_request(&listener);
            respond(
                stream,
                200,
                &json!({"sha":"before", "content":old}).to_string(),
            );
            let (stream, request) = read_request(&listener);
            assert!(request.starts_with("PUT "));
            let body: JsonValue =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["sha"], "before");
            let encoded = STANDARD.decode(body["content"].as_str().unwrap()).unwrap();
            let plain = decrypt_bundle("test-sync-secret", &encoded).unwrap();
            let bundle: JsonValue = serde_json::from_slice(&plain).unwrap();
            assert_eq!(bundle["items"].as_array().unwrap().len(), 2);
            assert!(bundle["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == "only-on-another-device"));
            respond(stream, 409, "conflict");
            let (stream, request) = read_request(&listener);
            assert!(request.starts_with("GET "));
            respond(
                stream,
                200,
                &json!({"sha":"after", "content":new}).to_string(),
            );
        });
        let view = sync.run_sync().unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(file).unwrap(), b"remote won race");
        assert_eq!(view.status.remote_sha.as_deref(), Some("after"));
        assert_eq!(view.records[0].downloaded_count, 1);
    }

    #[test]
    fn local_edit_during_download_is_not_overwritten_and_failure_is_recorded() {
        let (_dir, sync, file, id, listener) = setup();
        let target = file.clone();
        let encrypted = encrypted_item(&id, "remote newer", "2026-01-02T00:00:00Z");
        let server = thread::spawn(move || {
            let (stream, _) = read_request(&listener);
            fs::write(target, b"edited during sync").unwrap();
            respond(
                stream,
                200,
                &json!({"sha":"remote-sha", "content":encrypted}).to_string(),
            );
        });
        assert_eq!(sync.run_sync().unwrap_err().code, "SYNC_LOCAL_CHANGED");
        server.join().unwrap();
        assert_eq!(fs::read(file).unwrap(), b"edited during sync");
        assert_eq!(
            sync.view().records[0].error_code.as_deref(),
            Some("SYNC_LOCAL_CHANGED")
        );
        assert_eq!(
            sync.view().records[0].remote_sha.as_deref(),
            Some("remote-sha")
        );
    }

    #[test]
    fn wrong_secret_fails_without_overwriting_either_side() {
        let (_dir, sync, file, id, listener) = setup();
        let encoded = encrypted_item(&id, "new", "2026-01-02T00:00:00Z");
        sync.save_settings("example/configs", None, Some("different-secret".into()))
            .unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = read_request(&listener);
            respond(
                stream,
                200,
                &json!({"sha":"remote-sha", "content":encoded}).to_string(),
            );
        });
        assert_eq!(sync.run_sync().unwrap_err().code, "SYNC_DECRYPT");
        server.join().unwrap();
        assert_eq!(fs::read(file).unwrap(), b"local");
        assert_eq!(sync.view().records[0].state, SyncState::Error);
    }

    #[test]
    fn repeated_github_races_stop_after_three_attempts() {
        let (_dir, sync, file, id, listener) = setup();
        let encrypted = encrypted_item(&id, "old", "2025-12-01T00:00:00Z");
        let server = thread::spawn(move || {
            for _ in 0..3 {
                let (stream, _) = read_request(&listener);
                respond(
                    stream,
                    200,
                    &json!({"sha":"old", "content":encrypted}).to_string(),
                );
                let (stream, request) = read_request(&listener);
                assert!(request.starts_with("PUT "));
                respond(stream, 409, "conflict");
            }
        });
        assert_eq!(sync.run_sync().unwrap_err().code, "GITHUB_CONFLICT");
        server.join().unwrap();
        assert_eq!(fs::read(file).unwrap(), b"local");
        assert_eq!(sync.view().records.len(), 1);
    }

    #[test]
    fn registered_application_receives_newer_remote_config_with_a_digest_guard() {
        let (_dir, sync, file, _file_id, listener) = setup();
        let socket = file.with_file_name("application.sock");
        let callback = crate::agent::bind(&socket).unwrap();
        let view = sync
            .register_application(ApplicationRegistration {
                app_id: "editor".into(),
                app_name: "Editor".into(),
                config_id: "settings".into(),
                label: "Editor settings".into(),
                enabled: true,
                sync_interface: SyncInterface {
                    socket_path: socket.to_str().unwrap().into(),
                    path: "/config".into(),
                },
            })
            .unwrap();
        let id = view
            .items
            .iter()
            .find(|item| item.kind == SyncKind::Application)
            .unwrap()
            .id
            .clone();
        sync.set_application_auto_sync("editor", false).unwrap();
        sync.register_application(ApplicationRegistration {
            app_id: "other".into(),
            app_name: "Other".into(),
            config_id: "settings".into(),
            label: "Other settings".into(),
            enabled: true,
            sync_interface: SyncInterface {
                socket_path: file.with_file_name("other.sock").to_str().unwrap().into(),
                path: "/config".into(),
            },
        })
        .unwrap();
        let other = DeferredItem {
            id: "app:other:settings".into(),
            label: "Other settings".into(),
            kind: SyncKind::Application,
            reason: "SYNC_APPLICATION_OFFLINE".into(),
        };
        {
            let mut state = sync.lock_state().unwrap();
            state.deferred.push(other.clone());
            state.retry_attempts = 3;
        }
        let deadline = sync.view().next_sync_at;
        let bundle = json!({ "format":"ai-config-manager-sync-bundle-v1", "created_at":"2026-01-02T00:00:00Z", "items":[{
            "id":id, "label":"Editor settings", "kind":"application", "path":null,
            "updated_at":"2026-01-02T00:00:00Z", "content":STANDARD.encode("new app content")
        }] });
        let encrypted = STANDARD.encode(
            encrypt_bundle("test-sync-secret", &serde_json::to_vec(&bundle).unwrap()).unwrap(),
        );
        let github = thread::spawn(move || {
            let (stream, _) = read_request(&listener);
            respond(
                stream,
                200,
                &json!({"sha":"remote-app", "content":encrypted}).to_string(),
            );
        });
        let application = thread::spawn(move || {
            for index in 0..2 {
                let (mut stream, _) = callback.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut header = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    header.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                assert!(!header.contains("test-token"));
                assert!(!header.to_ascii_lowercase().contains("authorization:"));
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                drop(reader);
                if index == 0 {
                    assert!(header.starts_with("GET /config "));
                    let body = json!({"content": STANDARD.encode("old app content"), "updated_at":"2026-01-01T00:00:00Z"}).to_string();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                } else {
                    assert!(header.starts_with("PUT /config "));
                    let value: JsonValue = serde_json::from_slice(&body).unwrap();
                    assert_eq!(value["expected_sha256"], hash(b"old app content"));
                    assert_eq!(value["updated_at"], "2026-01-02T00:00:00Z");
                    assert_eq!(
                        STANDARD.decode(value["content"].as_str().unwrap()).unwrap(),
                        b"new app content"
                    );
                    write!(
                        stream,
                        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                }
            }
        });
        let result = sync.run_application("editor", SyncTrigger::Agent).unwrap();
        github.join().unwrap();
        application.join().unwrap();
        assert_eq!(result.records[0].downloaded_count, 1);
        assert_eq!(result.records[0].item_count, 1);
        assert_eq!(result.records[0].app_id.as_deref(), Some("editor"));
        assert_eq!(result.next_sync_at, deadline);
        assert_eq!(result.deferred, vec![other]);
        assert_eq!(sync.lock_state().unwrap().retry_attempts, 3);
        assert_eq!(fs::read(file).unwrap(), b"local");
    }

    #[test]
    fn scheduled_sync_skips_manual_only_applications_and_retains_their_remote_data() {
        let (_dir, sync, file, file_id, listener) = setup();
        sync.register_application(ApplicationRegistration {
            app_id: "manual".into(),
            app_name: "Manual".into(),
            config_id: "settings".into(),
            label: "Manual settings".into(),
            enabled: true,
            sync_interface: SyncInterface {
                socket_path: file.with_file_name("manual.sock").to_str().unwrap().into(),
                path: "/config".into(),
            },
        })
        .unwrap();
        sync.set_application_auto_sync("manual", false).unwrap();
        let encrypted = encrypted_item(
            "app:manual:settings",
            "remote manual settings",
            "2026-01-02T00:00:00Z",
        );
        let server = thread::spawn(move || {
            let (stream, _) = read_request(&listener);
            respond(
                stream,
                200,
                &json!({"sha":"before", "content":encrypted}).to_string(),
            );
            let (stream, request) = read_request(&listener);
            let body: JsonValue =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            let encoded = STANDARD.decode(body["content"].as_str().unwrap()).unwrap();
            let plain = decrypt_bundle("test-sync-secret", &encoded).unwrap();
            let bundle: JsonValue = serde_json::from_slice(&plain).unwrap();
            let items = bundle["items"].as_array().unwrap();
            assert_eq!(items.len(), 3);
            let manual = items
                .iter()
                .find(|item| item["id"] == "app:manual:settings")
                .unwrap();
            assert_eq!(
                STANDARD
                    .decode(manual["content"].as_str().unwrap())
                    .unwrap(),
                b"remote manual settings"
            );
            assert!(items.iter().any(|item| item["id"] == file_id));
            respond(stream, 201, &json!({"content":{"sha":"after"}}).to_string());
        });
        let due = DateTime::parse_from_rfc3339(sync.view().next_sync_at.as_deref().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        let view = sync.run_due_sync(due).unwrap().unwrap();
        server.join().unwrap();
        assert_eq!(view.records[0].item_count, 1);
        assert_eq!(view.records[0].app_id, None);
        assert!(view.deferred.is_empty());
        assert!(
            view.items
                .iter()
                .find(|item| item.id == "app:manual:settings")
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn manual_only_application_failure_is_recorded_without_automatic_retry() {
        let (_dir, sync, file, _file_id, listener) = setup();
        let socket = file.with_file_name("manual-only.sock");
        sync.register_application(ApplicationRegistration {
            app_id: "manual".into(),
            app_name: "Manual".into(),
            config_id: "settings".into(),
            label: "Manual settings".into(),
            enabled: true,
            sync_interface: SyncInterface {
                socket_path: socket.to_str().unwrap().into(),
                path: "/config".into(),
            },
        })
        .unwrap();
        sync.set_application_auto_sync("manual", false).unwrap();
        let bundle = json!({ "format":"ai-config-manager-sync-bundle-v1", "created_at":"2026-01-02T00:00:00Z", "items":[{
            "id":"app:manual:settings", "label":"Manual settings", "kind":"application", "path":null,
            "updated_at":"2026-01-02T00:00:00Z", "content":STANDARD.encode("remote manual settings")
        }] });
        let encrypted = STANDARD.encode(
            encrypt_bundle("test-sync-secret", &serde_json::to_vec(&bundle).unwrap()).unwrap(),
        );
        let server = thread::spawn(move || {
            let (stream, request) = read_request(&listener);
            assert!(request.starts_with("GET "));
            respond(
                stream,
                200,
                &json!({"sha":"remote-manual", "content":encrypted}).to_string(),
            );
        });
        let view = sync.run_application("manual", SyncTrigger::Manual).unwrap();
        server.join().unwrap();
        assert!(view.deferred.is_empty());
        assert_eq!(view.records[0].app_id.as_deref(), Some("manual"));
        assert_eq!(view.records[0].state, SyncState::Error);
        assert_eq!(
            view.records[0].error_code.as_deref(),
            Some("SYNC_APPLICATION_OFFLINE")
        );
    }

    #[test]
    fn offline_application_is_deferred_and_retried_without_blocking_files() {
        let (_dir, sync, file, _file_id, listener) = setup();
        let socket = file.with_file_name("offline.sock");
        sync.register_application(ApplicationRegistration {
            app_id: "offline".into(),
            app_name: "Offline".into(),
            config_id: "settings".into(),
            label: "Offline settings".into(),
            enabled: true,
            sync_interface: SyncInterface {
                socket_path: socket.to_str().unwrap().into(),
                path: "/config".into(),
            },
        })
        .unwrap();
        let app_id = sync
            .view()
            .items
            .iter()
            .find(|item| item.kind == SyncKind::Application)
            .unwrap()
            .id
            .clone();

        let server = thread::spawn(move || {
            for sha in ["remote-1", "remote-2"] {
                let (stream, request) = read_request(&listener);
                assert!(request.starts_with("GET "));
                respond(stream, 404, "");
                let (stream, request) = read_request(&listener);
                assert!(request.starts_with("PUT "));
                respond(stream, 201, &json!({"content":{"sha":sha}}).to_string());
            }
        });
        let view = sync.run_sync().unwrap();
        assert_eq!(view.records[0].state, SyncState::Success);
        assert_eq!(view.records[0].uploaded_count, 1);
        assert_eq!(view.deferred.len(), 1);
        assert_eq!(view.deferred[0].id, app_id);
        assert_eq!(view.deferred[0].reason, "SYNC_APPLICATION_OFFLINE");
        let retry = DateTime::parse_from_rfc3339(view.next_sync_at.as_deref().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        assert!(retry <= Utc::now() + Duration::from_secs(61));
        assert!(!fs::read_to_string(&sync.path)
            .unwrap()
            .contains("app content"));

        // Once the application is online, the next attempt uploads it and clears
        // the deferred entry.
        let callback = crate::agent::bind(&socket).unwrap();
        let application = thread::spawn(move || {
            let (mut stream, _) = callback.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut header = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                header.push_str(&line);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            assert!(header.starts_with("GET /config "));
            let reply = json!({
                "content": STANDARD.encode("app content"),
                "updated_at": "2026-01-01T00:00:00Z",
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        });
        let view = sync.run_sync().unwrap();
        server.join().unwrap();
        application.join().unwrap();
        assert!(view.deferred.is_empty());
        assert_eq!(view.records[0].state, SyncState::Success);
        assert_eq!(view.records[0].uploaded_count, 2);
    }
}
