use super::*;
const FIELDS: [&str; 5] = ["api_key", "base_url", "model", "encrypted", "configured"];
fn valid_field(field: &str) -> Option<&str> {
    if FIELDS.contains(&field) {
        return Some(field);
    }
    let (kind, name) = field.split_once('.')?;
    EndpointKind::ALL
        .iter()
        .any(|value| value.key() == kind)
        .then(|| FIELDS.contains(&name).then_some(name))
        .flatten()
}
pub(super) fn validate_history(h: &[HistoryEntry]) -> Result<()> {
    if h.len() > 10 {
        return Err(err("HISTORY_CORRUPT"));
    }
    for e in h {
        if e.id.len() != 32
            || !e.id.bytes().all(|b| b.is_ascii_hexdigit())
            || e.sha256.len() != 64
            || !e.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || chrono::DateTime::parse_from_rfc3339(&e.timestamp).is_err()
            || !matches!(e.source.as_str(), "desktop" | "cli" | "skill" | "test")
            || e.diff.len() > 20
        {
            return Err(err("HISTORY_CORRUPT"));
        }
        for d in &e.diff {
            let Some(name) = valid_field(&d.field) else {
                return Err(err("HISTORY_CORRUPT"));
            };
            if d.before.len() > 8192 || d.after.len() > 8192 {
                return Err(err("HISTORY_CORRUPT"));
            }
            if name == "api_key"
                && (!matches!(d.before.as_str(), "[REDACTED]" | "[EMPTY]")
                    || !matches!(d.after.as_str(), "[REDACTED]" | "[EMPTY]"))
            {
                return Err(err("HISTORY_CORRUPT"));
            }
        }
    }
    Ok(())
}
