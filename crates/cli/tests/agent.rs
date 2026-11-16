use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

fn invoke(home: &std::path::Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ai-config-manager-cli"))
        .env("HOME", home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn standalone_cli_rejects_secret_arguments_and_decrypted_reads() {
    let home = tempfile::tempdir().unwrap();
    let marker = "synthetic-secret-84231";
    for (args, input) in [
        (vec!["agent", marker], "".to_string()),
        (vec!["agent"], format!("{{\"op\":\"{marker}\"}}")),
        (vec!["agent"], "{\"op\":\"read_decrypted\"}".to_string()),
        (
            vec!["agent"],
            "{\"op\":\"read_endpoint_decrypted\",\"kind\":\"openai\"}".to_string(),
        ),
    ] {
        let output = invoke(home.path(), &args, &input);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(marker));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(marker));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], "INVALID_ARGUMENT");
    }
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    assert!(invoke(home.path(), &["--help"], "").status.success());
}

#[cfg(unix)]
#[test]
fn agent_uses_authenticated_socket_and_redacts_responses() {
    use ai_config_core::agent::{self, AgentOp};
    use serde_json::json;
    use std::thread;

    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".config/ai-config-manager");
    let socket = dir.join("agent.sock");
    let token = agent::new_token();
    let listener = agent::bind(&socket).unwrap();
    agent::write_token(&dir.join("agent.token"), &token).unwrap();
    let expected = token.clone();
    let server = thread::spawn(move || {
        for _ in 0..3 {
            let (stream, _) = listener.accept().unwrap();
            agent::serve_stream(stream, &expected, &|op| match op {
                AgentOp::SyncRead => Ok(json!({"items": [1], "nested": [{"api_key": "synthetic-returned-key", "token": "synthetic-returned-token"}]})),
                AgentOp::Save { request } => {
                    assert_eq!(request.api_key.as_deref(), Some("synthetic-stdin-key"));
                    Ok(json!({"model": request.model, "key_present": true}))
                }
                _ => panic!("unexpected request"),
            }).unwrap();
        }
    });
    let read = invoke(home.path(), &["agent"], r#"{"op":"sync_read"}"#);
    assert!(read.status.success());
    let result: serde_json::Value = serde_json::from_slice(&read.stdout).unwrap();
    assert_eq!(result["data"]["items"], json!([1]));
    assert!(result["data"]["nested"][0]["api_key"].is_null());
    assert!(!String::from_utf8_lossy(&read.stdout).contains("synthetic-returned"));
    assert!(!String::from_utf8_lossy(&read.stdout).contains(&token));

    let saved = invoke(
        home.path(),
        &["agent"],
        r#"{"op":"save","request":{"base_url":"https://example.invalid","model":"test","api_key":"synthetic-stdin-key","encrypt":false,"expected_revision":"missing"}}"#,
    );
    assert!(saved.status.success());
    assert!(!String::from_utf8_lossy(&saved.stdout).contains("synthetic-stdin-key"));
    assert!(!String::from_utf8_lossy(&saved.stderr).contains("synthetic-stdin-key"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&saved.stdout).unwrap()["data"]["model"],
        "test"
    );

    agent::write_token(&dir.join("agent.token"), &agent::new_token()).unwrap();
    let rejected = invoke(home.path(), &["agent"], r#"{"op":"sync_read"}"#);
    assert!(!rejected.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&rejected.stdout).unwrap()["error"]["code"],
        "UNAUTHORIZED"
    );
    server.join().unwrap();
}
