use std::{
    io::Write,
    process::{Command, Stdio},
};
use tempfile::TempDir;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ai-config"))
        .args(args)
        .output()
        .unwrap()
}

fn run_with_stdin(args: &[&str], input: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ai-config"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn help_and_invalid_argument_are_secret_free() {
    assert!(run(&["--help"]).status.success());
    let output = run(&["not-an-operation"]);
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn sync_rejects_direct_config_and_incomplete_registration_without_touching_files() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("ai.yml");
    for args in [
        vec!["sync", "--config", path.to_str().unwrap()],
        vec!["sync-status", "--config", path.to_str().unwrap()],
        vec!["sync-add"],
        vec!["sync-add", "--label", "Other"],
        vec!["read", "--path", path.to_str().unwrap()],
    ] {
        let output = run(&args);
        assert!(!output.status.success());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], "INVALID_ARGUMENT");
    }
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn read_plaintext_never_returns_key_and_check_is_offline() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("ai.yml");
    let marker = "synthetic-secret-never-print-987654321";
    std::fs::write(&path, format!("ai:\n  openai:\n    base_url: https://example.invalid/v1\n    api_key: {marker}\n    model: test-model\n")).unwrap();
    let output = run(&["read", "--config", path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(marker));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(marker));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["key_present"], true);
    assert_eq!(value["data"]["model"], "test-model");
    assert!(run(&["check", "--config", path.to_str().unwrap()])
        .status
        .success());
}

#[test]
fn set_preserves_the_existing_key_and_updates_direct_file() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("ai.yml");
    let marker = "synthetic-existing-key-112233";
    std::fs::write(&path, format!("ai:\n  openai:\n    base_url: https://example.invalid/v1\n    api_key: {marker}\n    model: old-model\n")).unwrap();
    let output = run(&[
        "set",
        "--model",
        "new-model",
        "--plaintext",
        "--config",
        path.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["model"], "new-model");
    assert_eq!(value["data"]["key_present"], true);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("new-model"));
    assert!(text.contains(marker));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(marker));
}

#[test]
fn set_reads_a_new_key_only_from_stdin() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("ai.yml");
    let marker = "synthetic-stdin-key-445566";
    std::fs::write(
        &path,
        "ai:\n  openai:\n    base_url: https://example.invalid/v1\n    model: old-model\n    api_key: old-key\n",
    )
    .unwrap();
    let output = run_with_stdin(
        &[
            "set",
            "--model",
            "stdin-model",
            "--api-key-stdin",
            "--plaintext",
            "--config",
            path.to_str().unwrap(),
        ],
        marker,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(marker));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(marker));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(marker));
    assert!(text.contains("stdin-model"));
}

#[test]
fn status_is_json_and_does_not_expose_a_token() {
    let output = run(&["status"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["ok"].as_bool().unwrap());
    assert!(value["data"]["socket"].is_string());
    assert!(value["data"]["token_present"].is_boolean());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("token\":\""));
}

#[test]
fn damaged_yaml_diagnostic_does_not_echo_input() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("ai.yml");
    let marker = "synthetic-secret-in-parser-error";
    std::fs::write(&path, format!("ai: [\n  {marker}: {{\n")).unwrap();
    let output = run(&["read", "--config", path.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(marker));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(marker));
}

#[test]
fn unsupported_encryption_is_reported_without_vault_access_or_secret_output() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("ai.yml");
    let fixture = "ai:\n  openai:\n    base_url: https://example.invalid/v1\n    model: encrypted-model\n    api_key: ENC(v99:invalid-envelope)\n";
    std::fs::write(&path, fixture).unwrap();
    let read = run(&["read", "--config", path.to_str().unwrap()]);
    assert!(read.status.success());
    let value: serde_json::Value = serde_json::from_slice(&read.stdout).unwrap();
    assert_eq!(value["data"]["encrypted"], true);
    assert!(!String::from_utf8_lossy(&read.stdout).contains("invalid-envelope"));
    let check = run(&["check", "--config", path.to_str().unwrap()]);
    assert!(!check.status.success());
    let error: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(error["error"]["code"], "DECRYPT_FAILED");
    assert_eq!(std::fs::read_to_string(path).unwrap(), fixture);
}

#[test]
fn named_endpoint_updates_do_not_touch_the_chat_block() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("ai.yml");
    let marker = "synthetic-chat-key-445566";
    std::fs::write(
        &path,
        format!(
            "ai:\n  openai:\n    base_url: https://example.invalid/v1\n    api_key: {marker}\n    model: chat-model\n"
        ),
    )
    .unwrap();
    let output = run(&[
        "set",
        "--endpoint",
        "embedding",
        "--base-url",
        "http://127.0.0.1:8848/v1",
        "--model",
        "embedding-model",
        "--plaintext",
        "--config",
        path.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let output = run(&[
        "read",
        "--endpoint",
        "embedding",
        "--config",
        path.to_str().unwrap(),
    ]);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["model"], "embedding-model");
    assert_eq!(value["data"]["key_present"], false);
    let output = run(&["read", "--config", path.to_str().unwrap()]);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["model"], "chat-model");
    let disk = std::fs::read_to_string(&path).unwrap();
    assert!(disk.contains(marker));
    assert!(disk.contains("embedding-model"));
}

#[test]
fn named_endpoint_requires_a_valid_name() {
    let output = run(&["read", "--endpoint", "vision"]);
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "INVALID_ARGUMENT");
}
