//! Secret-free command-line interface. No argument accepts credentials.
//!
//! Reads and writes are routed through the running desktop app when its local
//! agent socket is available. If the app is not running, the same Rust core
//! operates directly on the configuration file, so the CLI remains useful for
//! automation and recovery. A key can only arrive on stdin, never in argv.
use ai_config_core::agent::{self, AgentCall, AgentOp, AgentSaveRequest};
use ai_config_core::{ConfigView, EndpointKind, Error, NativeVault, Store};
use serde_json::{json, Value};
use std::{io::Read, path::PathBuf, sync::Arc};
use zeroize::{Zeroize, Zeroizing};

const HELP: &str =
    "ai-config-manager-cli <read|check|history|test|status|set|sync|sync-status|sync-add|agent> [options]\n\
JSON output never includes an API key. Default configuration: ~/.config/ai.yml\n\
read: redacted metadata; check: validate and decrypt locally; history: last 10 changes;\n\
test: explicit GET {base_url}/models network check; status: local agent availability;\n\
sync: upload registered configurations to the saved GitHub target (requires desktop);\n\
sync-status: show sync target, registered items and last result (requires desktop);\n\
sync-add: register a third-party file with --path PATH [--label LABEL] (requires desktop);\n\
agent: read one JSON AgentOp from stdin, e.g. {\"op\":\"sync_read\"} (requires desktop);\n\
       credential-bearing read_decrypted operations are prohibited. No agent arguments.\n\
       AgentOp fields and operation names follow the desktop socket protocol.\n\
--endpoint openai|embedding|laya|cosyvoice selects an AI interface (default openai)\n\
set: update fields. Options: --base-url URL --model MODEL --api-key-stdin\n\
     --encrypt (default) --plaintext --expected-revision REV [--config PATH]\n\
The API key is read from stdin only when --api-key-stdin is supplied. Exit 0 success; 1 failure.";

struct Args {
    operation: String,
    config: Option<PathBuf>,
    endpoint: EndpointKind,
    base_url: Option<String>,
    model: Option<String>,
    api_key_stdin: bool,
    encrypt: Option<bool>,
    expected_revision: Option<String>,
    path: Option<String>,
    label: Option<String>,
}

fn error(code: &str, message: &str) -> Error {
    Error {
        code: code.into(),
        message: message.into(),
    }
}

fn output(value: Value, success: bool) -> ! {
    println!("{value}");
    std::process::exit(if success { 0 } else { 1 });
}

fn failure(error: &Error) -> ! {
    output(
        json!({"ok":false,"error":{"code":error.code,"message":error.message}}),
        false,
    )
}

fn value<T: serde::Serialize>(value: &T) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|_| error("SERIALIZE", "Cannot encode the result."))
}

fn take_value(args: &[String], index: &mut usize, name: &str) -> Result<String, Error> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| error("INVALID_ARGUMENT", &format!("{name} requires a value.")))
}

fn parse(args: &[String]) -> Result<Args, Error> {
    let operation = args
        .first()
        .cloned()
        .ok_or_else(|| error("INVALID_ARGUMENT", "An operation is required. Use --help."))?;
    if ![
        "read",
        "check",
        "history",
        "test",
        "status",
        "set",
        "sync",
        "sync-status",
        "sync-add",
    ]
    .contains(&operation.as_str())
    {
        return Err(error(
            "INVALID_ARGUMENT",
            "Unknown operation. Use --help for available commands.",
        ));
    }
    let mut parsed = Args {
        operation: operation.clone(),
        config: None,
        endpoint: EndpointKind::Openai,
        base_url: None,
        model: None,
        api_key_stdin: false,
        encrypt: None,
        expected_revision: None,
        path: None,
        label: None,
    };
    let mut encrypt_flag = false;
    let mut plaintext_flag = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--config" => {
                parsed.config = Some(PathBuf::from(take_value(args, &mut index, "--config")?))
            }
            "--base-url" => parsed.base_url = Some(take_value(args, &mut index, "--base-url")?),
            "--model" => parsed.model = Some(take_value(args, &mut index, "--model")?),
            "--endpoint" => {
                let value = take_value(args, &mut index, "--endpoint")?;
                parsed.endpoint = EndpointKind::from_key(&value).ok_or_else(|| {
                    error(
                        "INVALID_ARGUMENT",
                        "--endpoint must be openai, embedding, laya, or cosyvoice.",
                    )
                })?;
            }
            "--path" => parsed.path = Some(take_value(args, &mut index, "--path")?),
            "--label" => parsed.label = Some(take_value(args, &mut index, "--label")?),
            "--expected-revision" => {
                parsed.expected_revision =
                    Some(take_value(args, &mut index, "--expected-revision")?)
            }
            "--api-key-stdin" => parsed.api_key_stdin = true,
            "--encrypt" => {
                if plaintext_flag {
                    return Err(error(
                        "INVALID_ARGUMENT",
                        "--encrypt and --plaintext cannot be combined.",
                    ));
                }
                encrypt_flag = true;
                parsed.encrypt = Some(true);
            }
            "--plaintext" => {
                if encrypt_flag {
                    return Err(error(
                        "INVALID_ARGUMENT",
                        "--encrypt and --plaintext cannot be combined.",
                    ));
                }
                plaintext_flag = true;
                parsed.encrypt = Some(false);
            }
            other => {
                return Err(error(
                    "INVALID_ARGUMENT",
                    &format!("Unknown option: {other}"),
                ))
            }
        }
        index += 1;
    }
    if operation != "set"
        && (parsed.base_url.is_some()
            || parsed.model.is_some()
            || parsed.api_key_stdin
            || parsed.encrypt.is_some()
            || parsed.expected_revision.is_some())
    {
        return Err(error(
            "INVALID_ARGUMENT",
            "Write options are only valid with set.",
        ));
    }
    if parsed.endpoint != EndpointKind::Openai
        && !matches!(operation.as_str(), "read" | "check" | "test" | "set")
    {
        return Err(error(
            "INVALID_ARGUMENT",
            "--endpoint is only valid with read, check, test, or set.",
        ));
    }
    if operation.starts_with("sync") && parsed.config.is_some() {
        return Err(error(
            "INVALID_ARGUMENT",
            "Sync uses the desktop's saved target; --config is not supported.",
        ));
    }
    if operation == "sync-add" {
        if parsed
            .path
            .as_ref()
            .is_none_or(|path| path.trim().is_empty())
        {
            return Err(error("INVALID_ARGUMENT", "sync-add requires --path PATH."));
        }
    } else if parsed.path.is_some() || parsed.label.is_some() {
        return Err(error(
            "INVALID_ARGUMENT",
            "--path and --label are only valid with sync-add.",
        ));
    }
    Ok(parsed)
}

fn direct_store(path: Option<PathBuf>) -> Result<Store, Error> {
    let path = match path {
        Some(path) => path,
        None => Store::default_path()?,
    };
    Ok(Store::new(path, Arc::new(NativeVault::new())))
}

fn via_agent(op: AgentOp) -> Option<Result<Value, Error>> {
    match agent::try_request(op) {
        AgentCall::Unavailable => None,
        AgentCall::Reply(reply) => Some(reply.into_result()),
        AgentCall::Failure(error) => Some(Err(error)),
    }
}

fn redact_agent_value(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(
                    key.as_str(),
                    "api_key" | "github_token" | "encryption_secret" | "token"
                ) {
                    wipe_value(value);
                    *value = Value::Null;
                } else {
                    redact_agent_value(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_agent_value),
        _ => {}
    }
}

fn wipe_value(value: &mut Value) {
    match value {
        Value::String(s) => s.zeroize(),
        Value::Array(items) => items.iter_mut().for_each(wipe_value),
        Value::Object(fields) => fields.values_mut().for_each(wipe_value),
        _ => {}
    }
}

fn run_agent() -> Result<Value, Error> {
    const MAX_INPUT: u64 = 2 * 1024 * 1024;
    let mut input = Zeroizing::new(String::new());
    std::io::stdin()
        .take(MAX_INPUT + 1)
        .read_to_string(&mut input)
        .map_err(|_| error("INVALID_ARGUMENT", "Cannot read agent JSON from stdin."))?;
    if input.len() as u64 > MAX_INPUT {
        return Err(error("TOO_LARGE", "Agent JSON exceeds the size limit."));
    }
    let op: AgentOp = serde_json::from_str(&input)
        .map_err(|_| error("INVALID_ARGUMENT", "Invalid agent JSON."))?;
    if matches!(
        op,
        AgentOp::ReadDecrypted | AgentOp::ReadEndpointDecrypted { .. }
    ) {
        return Err(error(
            "INVALID_ARGUMENT",
            "Credential-bearing reads are not available in the CLI.",
        ));
    }
    let mut data = match via_agent(op) {
        Some(Ok(data)) => data,
        Some(Err(error)) => return Err(error),
        None => {
            return Err(error(
                "AGENT_UNAVAILABLE",
                "Start the desktop app before using agent.",
            ))
        }
    };
    redact_agent_value(&mut data);
    Ok(data)
}

fn run(args: Args) -> Result<Value, Error> {
    match args.operation.as_str() {
        "read" => {
            if args.config.is_none() {
                let op = if args.endpoint == EndpointKind::Openai {
                    AgentOp::Read
                } else {
                    AgentOp::ReadEndpoint {
                        kind: args.endpoint,
                    }
                };
                if let Some(result) = via_agent(op) {
                    return result;
                }
            }
            value(&direct_store(args.config)?.load_endpoint(args.endpoint)?)
        }
        "check" => {
            if args.config.is_none() {
                let op = if args.endpoint == EndpointKind::Openai {
                    AgentOp::Check
                } else {
                    AgentOp::CheckEndpoint {
                        kind: args.endpoint,
                    }
                };
                if let Some(result) = via_agent(op) {
                    return result;
                }
            }
            direct_store(args.config)?.validate_endpoint(args.endpoint)?;
            Ok(json!({"valid":true}))
        }
        "history" => {
            if args.config.is_none() {
                if let Some(result) = via_agent(AgentOp::History) {
                    return result;
                }
            }
            value(&direct_store(args.config)?.history()?)
        }
        "test" => {
            if args.config.is_none() {
                let op = if args.endpoint == EndpointKind::Openai {
                    AgentOp::Test
                } else {
                    AgentOp::TestEndpoint {
                        kind: args.endpoint,
                    }
                };
                if let Some(result) = via_agent(op) {
                    return result;
                }
            }
            value(&direct_store(args.config)?.test_endpoint_kind(args.endpoint)?)
        }
        "status" => {
            let paths = agent::paths()?;
            let running = agent::is_running();
            let mut token = agent::read_token(&paths.token).ok();
            let token_present = token.is_some();
            if let Some(token) = token.as_mut() {
                token.zeroize();
            }
            let config = match &args.config {
                Some(path) => path.clone(),
                None => Store::default_path()?,
            };
            Ok(json!({
                "running": running,
                "socket": paths.socket,
                "token_present": token_present,
                "config": config
            }))
        }
        "set" => set(args),
        "sync" | "sync-status" | "sync-add" => {
            let op = match args.operation.as_str() {
                "sync" => AgentOp::SyncRun,
                "sync-status" => AgentOp::SyncRead,
                _ => AgentOp::SyncAddItem {
                    path: args.path.expect("validated sync-add path"),
                    label: args.label,
                    enabled: true,
                },
            };
            via_agent(op).unwrap_or_else(|| {
                Err(error(
                    "AGENT_UNAVAILABLE",
                    "Start the desktop app before registering or syncing configurations.",
                ))
            })
        }
        _ => unreachable!(),
    }
}

fn set(args: Args) -> Result<Value, Error> {
    let encrypt = args.encrypt.unwrap_or(true);
    let api_key = if args.api_key_stdin {
        Some(read_secret_stdin()?)
    } else {
        None
    };
    if args.config.is_none() {
        let read = if args.endpoint == EndpointKind::Openai {
            AgentOp::Read
        } else {
            AgentOp::ReadEndpoint {
                kind: args.endpoint,
            }
        };
        match via_agent(read) {
            Some(Ok(data)) => {
                let view: ConfigView = serde_json::from_value(data)
                    .map_err(|_| error("SERIALIZE", "Cannot decode the current configuration."))?;
                let request = build_request(&args, &view, api_key, encrypt)?;
                let save = if args.endpoint == EndpointKind::Openai {
                    AgentOp::Save { request }
                } else {
                    AgentOp::SaveEndpoint {
                        kind: args.endpoint,
                        request,
                    }
                };
                if let Some(result) = via_agent(save) {
                    return result;
                }
                // The app exited after the read. Do not retry blindly: the
                // request may already have been committed with a lost reply.
                return Err(error(
                    "AGENT_UNAVAILABLE",
                    "The desktop app exited before the save. Re-run set after confirming the current configuration.",
                ));
            }
            Some(Err(error)) => return Err(error),
            None => {
                let store = direct_store(None)?;
                let view = store.load_endpoint(args.endpoint)?;
                let request = build_request(&args, &view, api_key, encrypt)?;
                return value(&store.save_endpoint(args.endpoint, request.into_save(), "cli")?);
            }
        }
    }
    let store = direct_store(args.config.clone())?;
    let view = store.load_endpoint(args.endpoint)?;
    let request = build_request(&args, &view, api_key, encrypt)?;
    value(&store.save_endpoint(args.endpoint, request.into_save(), "cli")?)
}

fn build_request(
    args: &Args,
    view: &ConfigView,
    api_key: Option<String>,
    encrypt: bool,
) -> Result<AgentSaveRequest, Error> {
    Ok(AgentSaveRequest {
        base_url: args
            .base_url
            .clone()
            .unwrap_or_else(|| view.base_url.clone()),
        model: args.model.clone().unwrap_or_else(|| view.model.clone()),
        api_key,
        encrypt,
        expected_revision: args
            .expected_revision
            .clone()
            .unwrap_or_else(|| view.revision.clone()),
    })
}

fn read_secret_stdin() -> Result<String, Error> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|_| error("INVALID_KEY", "Could not read the API key from stdin."))?;
    let secret = input.trim().to_string();
    input.zeroize();
    if secret.is_empty() {
        return Err(error("INVALID_KEY", "The API key from stdin is empty."));
    }
    if secret.chars().any(char::is_control) {
        return Err(error(
            "INVALID_KEY",
            "The API key from stdin contains control characters.",
        ));
    }
    Ok(secret)
}

pub(crate) fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args == ["--help"] || args == ["help"] {
        println!("{HELP}");
        return;
    }
    if args.first().is_some_and(|arg| arg == "agent") {
        if args.len() != 1 {
            failure(&error(
                "INVALID_ARGUMENT",
                "agent accepts JSON on stdin only.",
            ));
        }
        match run_agent() {
            Ok(data) => output(json!({"ok":true,"data":data}), true),
            Err(error) => failure(&error),
        }
    }
    let parsed = match parse(&args) {
        Ok(parsed) => parsed,
        Err(error) => failure(&error),
    };
    match run(parsed) {
        Ok(data) => output(json!({"ok":true,"data":data}), true),
        Err(error) => failure(&error),
    }
}
