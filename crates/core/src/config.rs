use super::*;
pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn parse(text: &str) -> Result<Value> {
    let mut docs = serde_yaml::Deserializer::from_str(text);
    let first = docs.next().ok_or_else(|| err("INVALID_CONFIG"))?;
    let value = Value::deserialize(first).map_err(|_| err("INVALID_CONFIG"))?;
    if docs.next().is_some() || !value.is_mapping() {
        return Err(err("INVALID_CONFIG"));
    }
    check_value(&value, 0)?;
    for kind in EndpointKind::ALL {
        if let Some(block) = endpoint(&value, kind)? {
            for name in ["base_url", "model", "api_key"] {
                if block
                    .get(Value::String(name.into()))
                    .is_some_and(|value| !value.is_string())
                {
                    return Err(err("INVALID_CONFIG"));
                }
            }
        }
    }
    Ok(value)
}
pub(super) fn check_value(v: &Value, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err(err("INVALID_CONFIG"));
    }
    match v {
        Value::Tagged(_) => Err(err("INVALID_CONFIG")),
        Value::Mapping(m) => {
            for (k, v) in m {
                if !k.is_string() {
                    return Err(err("INVALID_CONFIG"));
                }
                check_value(v, depth + 1)?
            }
            Ok(())
        }
        Value::Sequence(s) => {
            for v in s {
                check_value(v, depth + 1)?
            }
            Ok(())
        }
        Value::Number(n) if n.as_f64().is_some_and(|n| !n.is_finite()) => {
            Err(err("INVALID_CONFIG"))
        }
        _ => Ok(()),
    }
}
pub(super) fn endpoint(doc: &Value, kind: EndpointKind) -> Result<Option<&Mapping>> {
    let root = doc.as_mapping().ok_or_else(|| err("INVALID_CONFIG"))?;
    let Some(ai) = root.get(Value::String("ai".into())) else {
        return Ok(None);
    };
    let ai = ai.as_mapping().ok_or_else(|| err("INVALID_CONFIG"))?;
    let Some(block) = ai.get(Value::String(kind.key().into())) else {
        return Ok(None);
    };
    Ok(Some(
        block.as_mapping().ok_or_else(|| err("INVALID_CONFIG"))?,
    ))
}
pub(super) fn field_in<'a>(doc: &'a Value, kind: EndpointKind, name: &str) -> Result<&'a str> {
    match endpoint(doc, kind)?.and_then(|m| m.get(Value::String(name.into()))) {
        None => Ok(""),
        Some(Value::String(s)) => Ok(s),
        _ => Err(err("INVALID_CONFIG")),
    }
}
pub(super) fn view(
    doc: &Value,
    kind: EndpointKind,
    revision: String,
    exists: bool,
) -> Result<ConfigView> {
    let key = field_in(doc, kind, "api_key")?;
    Ok(ConfigView {
        exists,
        base_url: field_in(doc, kind, "base_url")?.into(),
        model: field_in(doc, kind, "model")?.into(),
        key_present: !key.is_empty(),
        encrypted: key.starts_with("ENC("),
        revision,
        endpoint: kind,
        configured: endpoint(doc, kind)?.is_some(),
    })
}
pub(super) fn validate(url: &str, model: &str, key: &str, requires_key: bool) -> Result<()> {
    if url.len() > 8192 {
        return Err(err("INVALID_URL"));
    }
    let u = url::Url::parse(url).map_err(|_| err("INVALID_URL"))?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || url.trim() != url
        || url.chars().any(char::is_control)
        || !url.contains("://")
        || url.split_once("://").is_some_and(|(_, rest)| {
            rest.split(['/', '?', '#'])
                .next()
                .is_some_and(|authority| authority.contains('@'))
        })
    {
        return Err(err("INVALID_URL"));
    }
    if model.trim().is_empty() || model.len() > 1024 || model.chars().any(char::is_control) {
        return Err(err("INVALID_MODEL"));
    }
    if (requires_key && key.trim().is_empty())
        || (!key.is_empty() && key.trim().is_empty())
        || key.len() > 65536
        || key.chars().any(char::is_control)
        || key.starts_with("ENC(")
    {
        return Err(err("INVALID_KEY"));
    }
    Ok(())
}
pub(super) fn set_fields(
    doc: &mut Value,
    kind: EndpointKind,
    url: &str,
    model: &str,
    key: &str,
) -> Result<()> {
    let root = doc.as_mapping_mut().ok_or_else(|| err("INVALID_CONFIG"))?;
    let ai = root
        .entry(Value::String("ai".into()))
        .or_insert(Value::Mapping(Mapping::new()))
        .as_mapping_mut()
        .ok_or_else(|| err("INVALID_CONFIG"))?;
    let block = ai
        .entry(Value::String(kind.key().into()))
        .or_insert(Value::Mapping(Mapping::new()))
        .as_mapping_mut()
        .ok_or_else(|| err("INVALID_CONFIG"))?;
    for (k, v) in [("base_url", url), ("model", model), ("api_key", key)] {
        block.insert(Value::String(k.into()), Value::String(v.into()));
    }
    Ok(())
}
pub(super) fn canonical_json(doc: &Value) -> Result<serde_json::Value> {
    fn norm(v: &Value) -> Result<serde_json::Value> {
        match v {
            Value::Mapping(m) => {
                let mut out = serde_json::Map::new();
                let mut entries = m
                    .iter()
                    .map(|(k, v)| Ok((k.as_str().ok_or_else(|| err("INVALID_CONFIG"))?, v)))
                    .collect::<Result<Vec<_>>>()?;
                entries.sort_by(|a, b| a.0.cmp(b.0));
                for (k, v) in entries {
                    out.insert(k.into(), norm(v)?);
                }
                Ok(serde_json::Value::Object(out))
            }
            Value::Sequence(s) => Ok(serde_json::Value::Array(
                s.iter().map(norm).collect::<Result<_>>()?,
            )),
            _ => serde_json::to_value(v).map_err(|_| err("INVALID_CONFIG")),
        }
    }
    norm(doc)
}
/// Decrypt every configured endpoint in place. The result is a full plaintext
/// document and must be wiped after use.
pub(super) fn decrypt_document(store: &Store, doc: &Value) -> Result<Value> {
    let mut plain = doc.clone();
    for kind in EndpointKind::ALL {
        let key = field_in(doc, kind, "api_key")?;
        if key.is_empty() {
            continue;
        }
        let secret = store.decrypt(key)?;
        set_fields(
            &mut plain,
            kind,
            field_in(doc, kind, "base_url")?,
            field_in(doc, kind, "model")?,
            &secret.0,
        )?;
    }
    Ok(plain)
}
/// Validate every endpoint that is present. A missing block is allowed; a
/// present block must be complete, and only OpenAI may omit the credential.
pub(super) fn validate_document(doc: &Value) -> Result<()> {
    let mut any = false;
    for kind in EndpointKind::ALL {
        if endpoint(doc, kind)?.is_none() {
            continue;
        }
        any = true;
        let key = field_in(doc, kind, "api_key")?;
        if key.starts_with("ENC(") {
            return Err(err("INVALID_CONFIG"));
        }
        validate(
            field_in(doc, kind, "base_url")?,
            field_in(doc, kind, "model")?,
            key,
            kind.requires_key(),
        )?;
    }
    if !any {
        return Err(err("INVALID_CONFIG"));
    }
    Ok(())
}
/// Re-encrypt each nonempty endpoint credential, or keep the plaintext when
/// encryption is explicitly disabled.
pub(super) fn store_document(
    store: &Store,
    plain: &Value,
    encrypt: bool,
    only: Option<EndpointKind>,
) -> Result<Value> {
    let mut stored = plain.clone();
    for kind in EndpointKind::ALL {
        if endpoint(plain, kind)?.is_none() {
            continue;
        }
        let key = field_in(plain, kind, "api_key")?;
        let value = if only.is_some_and(|target| target != kind) {
            // Untouched blocks keep their existing storage form.
            key.to_string()
        } else if key.is_empty() {
            String::new()
        } else if key.starts_with("ENC(") {
            if encrypt {
                key.to_string()
            } else {
                store.decrypt(key)?.0.clone()
            }
        } else if encrypt {
            store.encrypt(&Secret(key.into()))?
        } else {
            key.to_string()
        };
        let base_url = field_in(plain, kind, "base_url")?.to_string();
        let model = field_in(plain, kind, "model")?.to_string();
        set_fields(&mut stored, kind, &base_url, &model, &value)?;
    }
    Ok(stored)
}
pub(super) fn wipe_yaml(v: &mut Value) {
    match v {
        Value::String(s) => s.zeroize(),
        Value::Sequence(a) => a.iter_mut().for_each(wipe_yaml),
        Value::Mapping(m) => {
            for (_, value) in m.iter_mut() {
                wipe_yaml(value);
            }
        }
        _ => {}
    }
}
pub(super) fn wipe_json(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) => s.zeroize(),
        serde_json::Value::Array(a) => a.iter_mut().for_each(wipe_json),
        serde_json::Value::Object(o) => o.values_mut().for_each(wipe_json),
        _ => {}
    }
}
