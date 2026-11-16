use super::*;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;
use tempfile::TempDir;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(120);
const SMALL_OUTPUT: usize = 64 * 1024;

pub(super) fn validate_repo(input: &str) -> Result<String> {
    if input.is_empty()
        || input.len() > 4096
        || input.trim() != input
        || input.starts_with('-')
        || input.chars().any(char::is_control)
    {
        return Err(err("GIT_REPO"));
    }
    if Path::new(input).is_absolute() {
        // Rust canonical paths use the Windows verbatim prefix; Git for Windows
        // expects drive/UNC paths in its own URL/path parser.
        #[cfg(windows)]
        if let Some(path) = input.strip_prefix(r"\\?\") {
            return Ok(match path.strip_prefix(r"UNC\") {
                Some(path) => format!(r"\\{path}"),
                None => path.into(),
            });
        }
        return Ok(input.into());
    }
    if input.chars().any(char::is_whitespace) || input.contains('\\') || input.contains(['?', '#'])
    {
        return Err(err("GIT_REPO"));
    }
    if input.contains("://") {
        let url = url::Url::parse(input).map_err(|_| err("GIT_REPO"))?;
        if !matches!(url.scheme(), "https" | "ssh")
            || url.host_str().is_none_or(|host| host.starts_with('-'))
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || (url.scheme() == "https" && !url.username().is_empty())
            || (url.scheme() == "ssh" && !valid_ssh_user(url.username()))
        {
            return Err(err("GIT_REPO"));
        }
        return Ok(url.into());
    }
    let (authority, path) = input.split_once(':').ok_or_else(|| err("GIT_REPO"))?;
    let (user, host) = authority.split_once('@').ok_or_else(|| err("GIT_REPO"))?;
    if user.is_empty()
        || !valid_ssh_user(user)
        || host.is_empty()
        || host.starts_with('-')
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-".contains(&byte))
        || path.is_empty()
        || path.starts_with('-')
        || path.contains(['\\', ':'])
    {
        return Err(err("GIT_REPO"));
    }
    Ok(input.into())
}

fn valid_ssh_user(user: &str) -> bool {
    !user.starts_with('-')
        && user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub(super) fn validate_branch(input: &str) -> Result<String> {
    if input.is_empty()
        || input.len() > 255
        || matches!(input, "@" | "HEAD")
        || input.contains("..")
        || input.contains("@{")
        || input
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || "~^:?*[\\".contains(ch))
        || input.split('/').any(|part| {
            part.is_empty()
                || part.starts_with(['.', '-'])
                || part.ends_with('.')
                || part.ends_with(".lock")
        })
    {
        return Err(err("GIT_BRANCH"));
    }
    Ok(input.into())
}

pub(super) struct GitSnapshot {
    pub head: String,
    pub encrypted: Option<Vec<u8>>,
}

pub(super) struct GitProbe {
    pub readable: bool,
    pub writable: bool,
    pub cleanup_ok: bool,
    pub code: String,
    pub probe_ref: Option<String>,
}

pub(super) struct GitRepository {
    directory: TempDir,
    branch_ref: String,
    remote_url: String,
    authorization: Option<Zeroizing<String>>,
}

struct GitOutput {
    success: bool,
    stdout: Vec<u8>,
}

struct Budget(Instant);

impl Budget {
    fn new() -> Self {
        Self(Instant::now() + OPERATION_TIMEOUT)
    }

    fn deadline(&self) -> Result<Instant> {
        let now = Instant::now();
        if now >= self.0 {
            return Err(err("GIT_TIMEOUT"));
        }
        Ok(self.0.min(now + COMMAND_TIMEOUT))
    }
}

impl GitRepository {
    pub(super) fn new(repo: &str, branch: &str, github_token: Option<&str>) -> Result<Self> {
        let repo = validate_repo(repo)?;
        let branch = validate_branch(branch)?;
        let authorization = if let Some(token) = github_token {
            let github = url::Url::parse(&repo).is_ok_and(|url| {
                url.scheme() == "https"
                    && url.host_str() == Some("github.com")
                    && url.port_or_known_default() == Some(443)
            });
            if !github
                || token.is_empty()
                || token.len() > 16 * 1024
                || token.chars().any(char::is_control)
            {
                return Err(err("GIT_REPO"));
            }
            let credentials = Zeroizing::new(format!("x-access-token:{token}"));
            let encoded = Zeroizing::new(STANDARD.encode(credentials.as_bytes()));
            Some(Zeroizing::new(format!(
                "Authorization: Basic {}",
                &*encoded
            )))
        } else {
            None
        };
        let directory = tempfile::tempdir().map_err(|_| err("GIT_FAILED"))?;
        let repository = Self {
            directory,
            branch_ref: format!("refs/heads/{branch}"),
            remote_url: repo,
            authorization,
        };
        let budget = Budget::new();
        repository.checked(
            &["init", "--bare", "--template=", "."],
            None,
            SMALL_OUTPUT,
            "GIT_FAILED",
            &budget,
        )?;
        repository.checked(
            &[
                "config",
                "--local",
                "remote.origin.url",
                &repository.remote_url,
            ],
            None,
            SMALL_OUTPUT,
            "GIT_FAILED",
            &budget,
        )?;
        Ok(repository)
    }

    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command.current_dir(self.directory.path());
        // Keep HOME and the user's credential helper / SSH agent, but never inherit
        // repository selectors, injected config, tracing or interactive askpass.
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(key);
            }
        }
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "Never")
            .env("GIT_TRACE2", "0")
            .env("GIT_TRACE2_EVENT", "0")
            .env("GIT_TRACE2_PERF", "0")
            .env(
                "GIT_SSH_COMMAND",
                "ssh -oBatchMode=yes -oStrictHostKeyChecking=yes",
            )
            .env("GIT_SSH_VARIANT", "ssh")
            .env("GIT_AUTHOR_NAME", "AI Config Manager")
            .env("GIT_AUTHOR_EMAIL", "ai-config-manager@localhost")
            .env("GIT_COMMITTER_NAME", "AI Config Manager")
            .env("GIT_COMMITTER_EMAIL", "ai-config-manager@localhost");
        if let Some(authorization) = &self.authorization {
            command
                .env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraHeader")
                .env("GIT_CONFIG_VALUE_0", &**authorization);
        }
        command.arg("--no-pager");
        for setting in [
            "protocol.allow=never",
            "protocol.https.allow=always",
            "protocol.ssh.allow=always",
            "protocol.file.allow=always",
            "http.followRedirects=false",
            "core.hooksPath=disabled-hooks",
            "core.fsmonitor=false",
            "commit.gpgSign=false",
            "tag.gpgSign=false",
            "gc.auto=0",
            "maintenance.auto=false",
            "fetch.recurseSubmodules=false",
            "push.recurseSubmodules=no",
        ] {
            command.args(["-c", setting]);
        }
        command.arg("--git-dir").arg(self.directory.path());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        command
    }

    fn run(
        &self,
        arguments: &[&str],
        input: Option<&[u8]>,
        limit: usize,
        budget: &Budget,
    ) -> Result<GitOutput> {
        let deadline = budget.deadline()?;
        let mut child = self
            .command()
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                err(if error.kind() == std::io::ErrorKind::NotFound {
                    "GIT_UNAVAILABLE"
                } else {
                    "GIT_FAILED"
                })
            })?;
        let process_tree = ProcessTree::new(&child);
        let mut stdin = child.stdin.take().ok_or_else(|| err("GIT_FAILED"))?;
        let stdout = child.stdout.take().ok_or_else(|| err("GIT_FAILED"))?;
        let stderr = child.stderr.take().ok_or_else(|| err("GIT_FAILED"))?;
        let input = input.unwrap_or_default().to_vec();
        let (written_tx, written_rx) = mpsc::channel();
        thread::spawn(move || {
            let result = stdin.write_all(&input).is_ok();
            drop(stdin);
            let _ = written_tx.send(result);
        });
        let (stdout_tx, stdout_rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = stdout_tx.send(drain(stdout, limit));
        });
        let (stderr_tx, stderr_rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = stderr_tx.send(drain(stderr, 0));
        });
        let result = (|| {
            let status = loop {
                if Instant::now() >= deadline {
                    return Err(err("GIT_TIMEOUT"));
                }
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) => thread::sleep(Duration::from_millis(5)),
                    Err(_) => return Err(err("GIT_FAILED")),
                }
            };
            let (stdout, overflow) =
                receive(&stdout_rx, deadline)?.map_err(|_| err("GIT_FAILED"))?;
            receive(&stderr_rx, deadline)?.map_err(|_| err("GIT_FAILED"))?;
            let written = receive(&written_rx, deadline)?;
            if overflow || (status.success() && !written) {
                return Err(err("GIT_FAILED"));
            }
            Ok(GitOutput {
                success: status.success(),
                stdout,
            })
        })();
        if result.is_err() {
            process_tree.kill(&mut child);
            let _ = child.wait();
        }
        result
    }

    fn checked(
        &self,
        arguments: &[&str],
        input: Option<&[u8]>,
        limit: usize,
        code: &str,
        budget: &Budget,
    ) -> Result<Vec<u8>> {
        let output = self.run(arguments, input, limit, budget)?;
        if !output.success {
            return Err(err(code));
        }
        Ok(output.stdout)
    }

    fn remote_head(&self, reference: &str, budget: &Budget) -> Result<Option<String>> {
        // Explicit URLs bypass inherited remote.origin.url/pushurl configuration.
        let output = self.checked(
            &["ls-remote", "--refs", &self.remote_url, reference],
            None,
            SMALL_OUTPUT,
            "GIT_READ",
            budget,
        )?;
        let text = std::str::from_utf8(&output).map_err(|_| err("GIT_READ"))?;
        let mut head = None;
        for line in text.lines() {
            let (oid, name) = line.split_once('\t').ok_or_else(|| err("GIT_READ"))?;
            if name != reference || !valid_oid(oid) || head.is_some() {
                return Err(err("GIT_READ"));
            }
            head = Some(oid.into());
        }
        Ok(head)
    }

    pub(super) fn fetch(&self) -> Result<Option<GitSnapshot>> {
        self.fetch_with_budget(&Budget::new())
    }

    fn fetch_with_budget(&self, budget: &Budget) -> Result<Option<GitSnapshot>> {
        if self.remote_head(&self.branch_ref, budget)?.is_none() {
            return Ok(None);
        }
        self.checked(
            &[
                "fetch",
                "--depth=1",
                "--no-tags",
                "--no-recurse-submodules",
                &self.remote_url,
                &format!("+{}:refs/ai-config-manager/fetched", self.branch_ref),
            ],
            None,
            SMALL_OUTPUT,
            "GIT_READ",
            budget,
        )?;
        let head = oid_output(self.checked(
            &[
                "rev-parse",
                "--verify",
                "refs/ai-config-manager/fetched^{commit}",
            ],
            None,
            SMALL_OUTPUT,
            "GIT_READ",
            budget,
        )?)?;
        let entry = self.checked(
            &[
                "ls-tree",
                "-z",
                "--full-tree",
                &head,
                "--",
                SYNC_REMOTE_PATH,
            ],
            None,
            SMALL_OUTPUT,
            "GIT_READ",
            budget,
        )?;
        if entry.is_empty() {
            return Ok(Some(GitSnapshot {
                head,
                encrypted: None,
            }));
        }
        let entry = std::str::from_utf8(&entry).map_err(|_| err("GIT_READ"))?;
        let (metadata, path) = entry.split_once('\t').ok_or_else(|| err("GIT_READ"))?;
        let parts: Vec<_> = metadata.split(' ').collect();
        if parts.len() != 3
            || parts[1] != "blob"
            || !valid_oid(parts[2])
            || path != format!("{SYNC_REMOTE_PATH}\0")
        {
            return Err(err("GIT_READ"));
        }
        let size = self.checked(
            &["cat-file", "-s", parts[2]],
            None,
            SMALL_OUTPUT,
            "GIT_READ",
            budget,
        )?;
        let size: u64 = std::str::from_utf8(&size)
            .ok()
            .and_then(|size| size.trim().parse().ok())
            .filter(|size| *size <= MAX_BUNDLE_BYTES * 2)
            .ok_or_else(|| err("GIT_READ"))?;
        let encrypted = self.checked(
            &["cat-file", "blob", parts[2]],
            None,
            size as usize,
            "GIT_READ",
            budget,
        )?;
        if encrypted.len() as u64 != size {
            return Err(err("GIT_READ"));
        }
        Ok(Some(GitSnapshot {
            head,
            encrypted: Some(encrypted),
        }))
    }

    fn commit(&self, tree: &str, parent: Option<&str>, budget: &Budget) -> Result<String> {
        let mut args = vec![
            "commit-tree",
            tree,
            "-m",
            "Update encrypted AI configuration",
        ];
        if let Some(parent) = parent {
            args.extend(["-p", parent]);
        }
        oid_output(self.checked(&args, None, SMALL_OUTPUT, "GIT_WRITE", budget)?)
    }

    fn push_ref(
        &self,
        reference: &str,
        head: Option<&str>,
        expected: Option<&str>,
        budget: &Budget,
    ) -> Result<bool> {
        Ok(self
            .run(
                &[
                    "push",
                    "--porcelain",
                    "--no-verify",
                    &format!("--force-with-lease={reference}:{}", expected.unwrap_or("")),
                    &self.remote_url,
                    &format!("{}:{reference}", head.unwrap_or("")),
                ],
                None,
                SMALL_OUTPUT,
                budget,
            )?
            .success)
    }

    pub(super) fn push(&self, encrypted: &str, expected_head: Option<&str>) -> Result<String> {
        if encrypted.len() as u64 > MAX_BUNDLE_BYTES * 2
            || expected_head.is_some_and(|head| !valid_oid(head))
        {
            return Err(err("GIT_WRITE"));
        }
        let budget = Budget::new();
        let tree_source = expected_head.unwrap_or("--empty");
        self.checked(
            &["read-tree", tree_source],
            None,
            SMALL_OUTPUT,
            "GIT_WRITE",
            &budget,
        )?;
        let blob = oid_output(self.checked(
            &["hash-object", "-w", "--stdin"],
            Some(encrypted.as_bytes()),
            SMALL_OUTPUT,
            "GIT_WRITE",
            &budget,
        )?)?;
        self.checked(
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "100644",
                &blob,
                SYNC_REMOTE_PATH,
            ],
            None,
            SMALL_OUTPUT,
            "GIT_WRITE",
            &budget,
        )?;
        let tree =
            oid_output(self.checked(&["write-tree"], None, SMALL_OUTPUT, "GIT_WRITE", &budget)?)?;
        let commit = self.commit(&tree, expected_head, &budget)?;
        if self.push_ref(&self.branch_ref, Some(&commit), expected_head, &budget)? {
            return Ok(commit);
        }
        match self.remote_head(&self.branch_ref, &budget) {
            Ok(current) if current.as_deref() == Some(&commit) => Ok(commit),
            Ok(current) if current.as_deref() != expected_head => Err(err("GIT_CONFLICT")),
            _ => Err(err("GIT_WRITE")),
        }
    }

    pub(super) fn probe(&self) -> GitProbe {
        let budget = Budget::new();
        let mut result = GitProbe {
            readable: false,
            writable: false,
            cleanup_ok: true,
            code: "OK".into(),
            probe_ref: None,
        };
        if let Err(error) = self.fetch_with_budget(&budget) {
            result.code = error.code;
            return result;
        }
        result.readable = true;
        let commit = (|| {
            let tree = oid_output(self.checked(
                &["hash-object", "-t", "tree", "-w", "--stdin"],
                Some(&[]),
                SMALL_OUTPUT,
                "GIT_WRITE",
                &budget,
            )?)?;
            self.commit(&tree, None, &budget)
        })();
        let commit = match commit {
            Ok(commit) => commit,
            Err(error) => {
                result.code = error.code;
                return result;
            }
        };
        let reference = format!("refs/heads/ai-config-manager-probe/{}", random_id());
        if reference == self.branch_ref {
            result.code = "GIT_FAILED".into();
            return result;
        }
        match self.push_ref(&reference, Some(&commit), None, &budget) {
            Ok(true) => result.writable = true,
            Ok(false) => result.code = "GIT_WRITE".into(),
            Err(error) => result.code = error.code,
        }
        // A failed push can still have reached the server. Always inspect the ref
        // and use a lease so cleanup cannot delete an unrelated concurrent update.
        let cleanup = match self.remote_head(&reference, &budget) {
            Ok(None) => true,
            Ok(Some(current)) if current == commit => {
                let _ = self.push_ref(&reference, None, Some(&commit), &budget);
                matches!(self.remote_head(&reference, &budget), Ok(None))
            }
            _ => false,
        };
        if !cleanup {
            result.cleanup_ok = false;
            result.code = "GIT_CLEANUP".into();
            result.probe_ref = Some(reference);
        }
        result
    }
}

fn valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn oid_output(output: Vec<u8>) -> Result<String> {
    let text = std::str::from_utf8(&output).map_err(|_| err("GIT_FAILED"))?;
    let oid = text.trim();
    if !valid_oid(oid) {
        return Err(err("GIT_FAILED"));
    }
    Ok(oid.into())
}

// Continue draining after the cap, including stderr, so a noisy Git child cannot
// block on a full pipe or make the parent allocate unbounded command output.
fn drain(mut stream: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut overflow = false;
    let mut buffer = [0; 8192];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok((output, overflow));
        }
        let keep = read.min(limit.saturating_sub(output.len()));
        output.extend_from_slice(&buffer[..keep]);
        overflow |= keep != read;
    }
}

fn receive<T>(receiver: &mpsc::Receiver<T>, deadline: Instant) -> Result<T> {
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|error| {
            err(match error {
                mpsc::RecvTimeoutError::Timeout => "GIT_TIMEOUT",
                mpsc::RecvTimeoutError::Disconnected => "GIT_FAILED",
            })
        })
}

#[cfg(unix)]
struct ProcessTree(u32);

#[cfg(unix)]
impl ProcessTree {
    fn new(child: &Child) -> Self {
        Self(child.id())
    }

    fn kill(&self, child: &mut Child) {
        // Every command starts its own process group, including ssh/helpers.
        unsafe { libc::kill(-(self.0 as i32), libc::SIGKILL) };
        let _ = child.kill();
    }
}

#[cfg(windows)]
struct ProcessTree(*mut std::ffi::c_void);

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(
        attributes: *const std::ffi::c_void,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn AssignProcessToJobObject(job: *mut std::ffi::c_void, process: *mut std::ffi::c_void) -> i32;
    fn TerminateJobObject(job: *mut std::ffi::c_void, code: u32) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
impl ProcessTree {
    fn new(child: &Child) -> Self {
        use std::os::windows::io::AsRawHandle;
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if !job.is_null() && unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) } == 0 {
            unsafe { CloseHandle(job) };
            return Self(std::ptr::null_mut());
        }
        Self(job)
    }

    fn kill(&self, child: &mut Child) {
        if !self.0.is_null() {
            unsafe { TerminateJobObject(self.0, 1) };
        }
        let _ = child.kill();
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> TempDir {
        let repository = GitRepository {
            directory: tempfile::tempdir().unwrap(),
            branch_ref: "refs/heads/main".into(),
            remote_url: String::new(),
            authorization: None,
        };
        raw(&repository, &["init", "--bare", "--template=", "."], None);
        repository.directory
    }

    fn repository(directory: &TempDir) -> GitRepository {
        GitRepository::new(directory.path().to_str().unwrap(), "main", None).unwrap()
    }

    fn raw(repository: &GitRepository, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
        repository
            .checked(args, input, SMALL_OUTPUT, "GIT_FAILED", &Budget::new())
            .unwrap()
    }

    fn seed(repository: &GitRepository, path: &str, content: &[u8]) -> String {
        raw(repository, &["read-tree", "--empty"], None);
        let blob = oid_output(raw(
            repository,
            &["hash-object", "-w", "--stdin"],
            Some(content),
        ))
        .unwrap();
        raw(
            repository,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "100644",
                &blob,
                path,
            ],
            None,
        );
        let tree = oid_output(raw(repository, &["write-tree"], None)).unwrap();
        let commit = repository.commit(&tree, None, &Budget::new()).unwrap();
        assert!(repository
            .push_ref(&repository.branch_ref, Some(&commit), None, &Budget::new())
            .unwrap());
        commit
    }

    #[test]
    fn validates_repository_without_network_or_credentials() {
        for repo in [
            "https://example.com/team/config.git",
            "ssh://git@example.com:2222/team/config.git",
            "git@example.com:team/config.git",
        ] {
            assert_eq!(validate_repo(repo).unwrap(), repo);
        }
        let local = tempfile::tempdir().unwrap();
        assert!(validate_repo(local.path().to_str().unwrap()).is_ok());
        for repo in [
            "",
            "--upload-pack=evil",
            "http://example.com/repo",
            "ext::evil",
            "file:///tmp/repo",
            "git://example.com/repo",
            "relative/repo",
            "https://user@example.com/repo",
            "https://user:secret@example.com/repo",
            "ssh://git:secret@example.com/repo",
            "https://example.com/repo?token=secret",
            "ssh://git@example.com/repo#secret",
            "git@example.com:-evil",
            "-oProxyCommand=evil@example.com:repo",
            "https://example.com/\nrepo",
        ] {
            let error = validate_repo(repo).unwrap_err();
            assert_eq!(error.code, "GIT_REPO");
            assert!(!format!("{error:?}").contains("secret"));
        }
    }

    #[test]
    fn validates_safe_head_branch_names() {
        for branch in ["main", "team/config-v2", "release_1.2", "同步/main"] {
            assert_eq!(validate_branch(branch).unwrap(), branch);
        }
        for branch in [
            "", "-main", "HEAD", "@", "a:b", "a b", "a\nb", "a..b", "a@{b", "a\\b", "a.lock",
            "a.lock/b", ".a", "a/.b", "a.", "a/", "/a", "a//b", "a~b", "a^b", "a?b", "a*b", "a[b",
        ] {
            assert_eq!(validate_branch(branch).unwrap_err().code, "GIT_BRANCH");
        }
    }

    #[cfg(windows)]
    #[test]
    fn canonical_windows_paths_are_usable_by_git() {
        let remote = bare();
        let canonical = remote.path().canonicalize().unwrap();
        let repository = GitRepository::new(canonical.to_str().unwrap(), "main", None).unwrap();
        let result = repository.probe();
        assert!(result.readable && result.writable && result.cleanup_ok);
    }

    #[test]
    fn empty_repository_first_push_and_fetch_bytes() {
        let remote = bare();
        let repository = repository(&remote);
        assert!(repository.fetch().unwrap().is_none());
        let head = repository.push("encrypted-snapshot", None).unwrap();
        let snapshot = repository.fetch().unwrap().unwrap();
        assert_eq!(snapshot.head, head);
        assert_eq!(snapshot.encrypted.unwrap(), b"encrypted-snapshot");
        let missing = GitRepository::new(remote.path().to_str().unwrap(), "missing", None).unwrap();
        assert!(missing.fetch().unwrap().is_none());
    }

    #[test]
    fn preserves_other_files_and_uses_loaded_tree() {
        let remote = bare();
        let repository = repository(&remote);
        let initial = seed(&repository, "README.md", b"keep this file\n");
        let loaded = repository.fetch().unwrap().unwrap();
        assert_eq!(loaded.head, initial);
        assert!(loaded.encrypted.is_none());
        let head = repository.push("encrypted", Some(&initial)).unwrap();
        assert_eq!(
            raw(&repository, &["show", &format!("{head}:README.md")], None),
            b"keep this file\n"
        );
        assert_eq!(
            raw(&repository, &["rev-parse", &format!("{head}^1")], None),
            format!("{initial}\n").as_bytes()
        );
    }

    #[test]
    fn concurrent_push_is_a_lease_conflict() {
        let remote = bare();
        let first = repository(&remote);
        let initial = first.push("initial", None).unwrap();
        let second = repository(&remote);
        second.fetch().unwrap();
        let winner = first.push("winner", Some(&initial)).unwrap();
        assert_eq!(
            second.push("loser", Some(&initial)).unwrap_err().code,
            "GIT_CONFLICT"
        );
        let snapshot = second.fetch().unwrap().unwrap();
        assert_eq!(snapshot.head, winner);
        assert_eq!(snapshot.encrypted.unwrap(), b"winner");
        assert_eq!(
            second.push("create-race", None).unwrap_err().code,
            "GIT_CONFLICT"
        );
    }

    #[test]
    fn remote_config_cannot_override_the_validated_target() {
        let target = bare();
        let unrelated = bare();
        let repository = repository(&target);
        for setting in ["remote.origin.url", "remote.origin.pushurl"] {
            raw(
                &repository,
                &[
                    "config",
                    "--local",
                    setting,
                    unrelated.path().to_str().unwrap(),
                ],
                None,
            );
        }
        let head = repository.push("encrypted", None).unwrap();
        assert_eq!(repository.fetch().unwrap().unwrap().head, head);
        let unrelated =
            GitRepository::new(unrelated.path().to_str().unwrap(), "main", None).unwrap();
        assert!(unrelated.fetch().unwrap().is_none());
    }

    #[test]
    fn probe_creates_and_removes_only_its_own_branch() {
        let remote = bare();
        let repository = repository(&remote);
        let initial = repository.push("unchanged", None).unwrap();
        let result = repository.probe();
        assert!(result.readable && result.writable && result.cleanup_ok);
        assert_eq!(result.code, "OK");
        assert!(result.probe_ref.is_none());
        assert_eq!(repository.fetch().unwrap().unwrap().head, initial);
        let refs = raw(&repository, &["ls-remote", "--heads", "origin"], None);
        assert_eq!(
            String::from_utf8(refs).unwrap(),
            format!("{initial}\trefs/heads/main\n")
        );
    }

    #[test]
    fn probe_on_empty_repository_leaves_it_empty() {
        let remote = bare();
        let repository = repository(&remote);
        let result = repository.probe();
        assert!(result.readable && result.writable && result.cleanup_ok);
        assert_eq!(result.code, "OK");
        assert!(raw(&repository, &["ls-remote", "--heads", "origin"], None).is_empty());
    }

    #[test]
    fn github_token_only_uses_scoped_environment_and_is_never_persisted() {
        let token = "synthetic-github-token-secret";
        let repository =
            GitRepository::new("https://github.com/team/repo.git", "main", Some(token)).unwrap();
        let command = repository.command();
        assert!(command
            .get_args()
            .all(|arg| !arg.to_string_lossy().contains(token)));
        let config = fs::read_to_string(repository.directory.path().join("config")).unwrap();
        assert!(!config.contains(token));
        assert!(!config.contains("Authorization"));
        assert!(config.contains("https://github.com/team/repo.git"));
        assert!(command.get_envs().any(|(key, value)| {
            key == "GIT_CONFIG_KEY_0"
                && value == Some("http.https://github.com/.extraHeader".as_ref())
        }));
        assert!(GitRepository::new("https://example.com/repo.git", "main", Some(token)).is_err());
    }

    #[test]
    fn failed_read_is_not_an_empty_repository_and_output_is_redacted() {
        let missing = tempfile::tempdir().unwrap();
        let path = missing.path().join("synthetic-secret-does-not-exist.git");
        let repository = GitRepository::new(path.to_str().unwrap(), "main", None).unwrap();
        let error = repository.fetch().err().unwrap();
        assert_eq!(error.code, "GIT_READ");
        assert!(!format!("{error:?}").contains("synthetic-secret"));
        let result = repository.probe();
        assert!(!result.readable && !result.writable);
        assert_eq!(result.code, "GIT_READ");
    }

    #[test]
    fn fetch_rejects_a_tree_or_oversized_snapshot() {
        let tree_remote = bare();
        let tree_repository = repository(&tree_remote);
        seed(
            &tree_repository,
            &format!("{SYNC_REMOTE_PATH}/nested"),
            b"invalid",
        );
        assert_eq!(tree_repository.fetch().err().unwrap().code, "GIT_READ");

        let large_remote = bare();
        let large_repository = repository(&large_remote);
        let oversized = vec![b'x'; (MAX_BUNDLE_BYTES * 2 + 1) as usize];
        seed(&large_repository, SYNC_REMOTE_PATH, &oversized);
        assert_eq!(large_repository.fetch().err().unwrap().code, "GIT_READ");
    }

    #[test]
    fn command_output_over_limit_is_rejected() {
        let remote = bare();
        let repository = repository(&remote);
        assert_eq!(
            repository
                .run(&["version"], None, 1, &Budget::new())
                .err()
                .unwrap()
                .code,
            "GIT_FAILED"
        );
    }

    #[cfg(unix)]
    fn receive_hook(remote: &TempDir, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(remote.path().join("hooks")).unwrap();
        let path = remote.path().join("hooks/pre-receive");
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn receive_hook_write_rejection_has_fixed_error_and_no_residual_probe() {
        let remote = bare();
        let repository = repository(&remote);
        receive_hook(
            &remote,
            "#!/bin/sh\nhead -c 131072 /dev/zero >&2\necho synthetic-secret >&2\nexit 1\n",
        );
        let error = repository.push("encrypted", None).unwrap_err();
        assert_eq!(error.code, "GIT_WRITE");
        assert!(!format!("{error:?}").contains("synthetic-secret"));
        let probe = repository.probe();
        assert!(probe.readable && !probe.writable && probe.cleanup_ok);
        assert_eq!(probe.code, "GIT_WRITE");
        assert!(probe.probe_ref.is_none());
        assert!(repository.fetch().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn receive_hook_delete_rejection_reports_residual_ref() {
        let remote = bare();
        let repository = repository(&remote);
        let initial = repository.push("unchanged", None).unwrap();
        receive_hook(
            &remote,
            "#!/bin/sh\nwhile read old new ref; do\ncase \"$new\" in\n0000000000000000000000000000000000000000) exit 1;;\nesac\ndone\n",
        );
        let result = repository.probe();
        assert!(result.readable && result.writable && !result.cleanup_ok);
        assert_eq!(result.code, "GIT_CLEANUP");
        let reference = result.probe_ref.unwrap();
        assert!(reference.starts_with("refs/heads/ai-config-manager-probe/"));
        let probe_head = repository
            .remote_head(&reference, &Budget::new())
            .unwrap()
            .unwrap();
        assert!(raw(&repository, &["ls-tree", "-r", &probe_head], None).is_empty());
        assert_eq!(repository.fetch().unwrap().unwrap().head, initial);
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_git_and_pipe_holding_descendants() {
        let remote = bare();
        let repository = repository(&remote);
        let started = Instant::now();
        let budget = Budget(started + Duration::from_millis(200));
        let error = repository
            .run(
                &["-c", "alias.synthetic-sleep=!sleep 30", "synthetic-sleep"],
                None,
                SMALL_OUTPUT,
                &budget,
            )
            .err()
            .unwrap();
        assert_eq!(error.code, "GIT_TIMEOUT");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
