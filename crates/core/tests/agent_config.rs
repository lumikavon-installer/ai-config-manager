use ai_config_core::{agent, Error, Result, SaveRequest, SecretKey, Store, Vault};
use serde_json::json;
use std::sync::{Arc, Mutex};

struct MemoryVault(Mutex<Option<[u8; 32]>>);
impl Vault for MemoryVault {
    fn get_key(&self) -> Result<Option<SecretKey>> {
        Ok(self.0.lock().unwrap().map(SecretKey::new))
    }
    fn create_key(&self) -> Result<SecretKey> {
        Ok(SecretKey::new(
            *self.0.lock().unwrap().get_or_insert([7; 32]),
        ))
    }
}

#[test]
fn decrypted_wire_data_uses_a_validated_snapshot_and_keeps_read_redacted() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Arc::new(MemoryVault(Mutex::new(None)));
    let path = directory.path().canonicalize().unwrap().join("ai.yml");
    let store = Store::new(path.clone(), vault.clone());
    store
        .save(SaveRequest {
            base_url: "https://example.com/v1".into(),
            model: "synthetic-model".into(),
            api_key: Some("sk-synthetic-agent".into()),
            encrypt: true,
            expected_revision: "missing".into(),
        })
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let redacted = serde_json::to_value(store.load().unwrap()).unwrap();
    assert!(redacted.get("api_key").is_none());
    assert_eq!(
        agent::read_decrypted_config(&store).unwrap(),
        json!({
            "base_url": "https://example.com/v1", "model": "synthetic-model",
            "api_key": "sk-synthetic-agent", "encrypted": true, "endpoint": "openai",
        })
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    *vault.0.lock().unwrap() = None;
    let response = agent::decrypted_config_response(&store);
    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["error"]["code"], "KEY_MISSING");
    assert!(!String::from_utf8_lossy(&response.body).contains("sk-synthetic-agent"));
    assert!(vault.0.lock().unwrap().is_none());
}

#[test]
fn missing_and_invalid_configs_do_not_produce_credential_responses() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap().join("ai.yml");
    let store = Store::new(path.clone(), Arc::new(MemoryVault(Mutex::new(None))));
    let error: Error = agent::read_decrypted_config(&store).unwrap_err();
    assert_eq!(error.code, "INVALID_URL");
    std::fs::write(path, "ai: {openai: {api_key: 'ENC(v2:bad)'}}").unwrap();
    assert_eq!(
        agent::read_decrypted_config(&store).unwrap_err().code,
        "DECRYPT_FAILED"
    );
}
