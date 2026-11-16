use crate::{canonical, err, platform, ConfigAdapter, Result, Sensitive};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

pub(crate) const EPOCH: &str = "1970-01-01T00:00:00Z";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    sha256: String,
    updated_at: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    current: Stamp,
    pending: Option<Stamp>,
}

pub(crate) struct State {
    path: PathBuf,
    metadata: Metadata,
    dirty: bool,
}

impl State {
    pub(crate) fn load(path: &Path, adapter: &dyn ConfigAdapter) -> Result<Self> {
        let value = Sensitive(adapter.read().map_err(|_| err("SYNC_CONFIG"))?);
        let sha256 = crate::digest(&value.0)?;
        let bytes = platform::read(path, 4096)?;
        let metadata: Metadata = match bytes {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| err("SYNC_CONFIG"))?,
            None => Metadata {
                current: Stamp {
                    sha256: sha256.clone(),
                    updated_at: EPOCH.into(),
                },
                pending: None,
            },
        };
        for stamp in std::iter::once(&metadata.current).chain(metadata.pending.iter()) {
            if !valid_digest(&stamp.sha256) || !valid_time(&stamp.updated_at) {
                return Err(err("SYNC_CONFIG"));
            }
        }
        let mut state = Self {
            path: path.into(),
            metadata,
            dirty: true,
        };
        state.observe(&sha256)?;
        Ok(state)
    }

    fn persist(&mut self) -> Result<()> {
        if self.dirty {
            let bytes = serde_json::to_vec(&self.metadata).map_err(|_| err("SYNC_CONFIG"))?;
            platform::atomic_write(&self.path, &bytes)?;
            self.dirty = false;
        }
        Ok(())
    }

    fn observe(&mut self, sha256: &str) -> Result<()> {
        if let Some(pending) = self.metadata.pending.take() {
            // The journal was durable before apply. On an ambiguous commit,
            // retain the imported timestamp even if an adapter normalized JSON.
            // A failed apply leaving the previous digest keeps its old timestamp.
            if sha256 != self.metadata.current.sha256 || sha256 == pending.sha256 {
                self.metadata.current = Stamp {
                    sha256: sha256.into(),
                    updated_at: pending.updated_at,
                };
            }
            self.dirty = true;
        } else if sha256 != self.metadata.current.sha256 {
            let previous = DateTime::parse_from_rfc3339(&self.metadata.current.updated_at)
                .map_err(|_| err("SYNC_CONFIG"))?
                .with_timezone(&Utc);
            let next = previous
                .checked_add_signed(chrono::Duration::nanoseconds(1))
                .ok_or_else(|| err("SYNC_CONFIG"))?;
            self.metadata.current = Stamp {
                sha256: sha256.into(),
                updated_at: Utc::now().max(next).to_rfc3339(),
            };
            self.dirty = true;
        }
        self.persist()
    }

    pub(crate) fn snapshot(
        &mut self,
        adapter: &dyn ConfigAdapter,
    ) -> Result<(Zeroizing<Vec<u8>>, String)> {
        let value = Sensitive(adapter.read().map_err(|_| err("SYNC_CONFIG"))?);
        let bytes = canonical(&value.0)?;
        self.observe(&crate::hash(&bytes))?;
        Ok((bytes, self.metadata.current.updated_at.clone()))
    }

    pub(crate) fn apply(
        &mut self,
        adapter: &dyn ConfigAdapter,
        value: serde_json::Value,
        expected: &str,
        timestamp: &str,
    ) -> Result<()> {
        let mut value = Sensitive(value);
        if !valid_digest(expected) || !valid_time(timestamp) {
            return Err(err("SYNC_CONFIG"));
        }
        let incoming = crate::digest(&value.0)?;
        let (current, _) = self.snapshot(adapter)?;
        if crate::hash(&current) != expected {
            return Err(err("SYNC_LOCAL_CHANGED"));
        }
        self.metadata.pending = Some(Stamp {
            sha256: incoming,
            updated_at: timestamp.into(),
        });
        self.dirty = true;
        // Never mutate application storage until its recovery stamp is durable.
        self.persist()?;
        if let Err(code) = adapter.apply(std::mem::take(&mut value.0), expected) {
            let code = crate::adapter_error(&code);
            if code == "SYNC_LOCAL_CHANGED" {
                // The adapter's CAS rejection is a definite non-commit.
                self.metadata.pending = None;
                self.dirty = true;
                self.persist()?;
            }
            return Err(code);
        }
        // Resolve through an actual read, including adapters that normalize data.
        // If this fails, the durable pending stamp survives process restart.
        self.snapshot(adapter)?;
        Ok(())
    }
}

pub(crate) fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn valid_time(value: &str) -> bool {
    value.len() <= 64 && DateTime::parse_from_rfc3339(value).is_ok()
}
