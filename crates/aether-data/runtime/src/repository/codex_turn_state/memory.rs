use std::collections::BTreeMap;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::DataLayerError;
use aether_data_contracts::repository::codex_turn_state::{
    CodexTurnStateBucketReadRepository, CodexTurnStateBucketWriteRepository,
    StoredCodexTurnStateBucket, UpsertCodexTurnStateBucket,
};

#[derive(Debug, Default)]
pub struct InMemoryCodexTurnStateBucketRepository {
    buckets: RwLock<BTreeMap<(String, String), StoredCodexTurnStateBucket>>,
}

impl InMemoryCodexTurnStateBucketRepository {
    pub fn seed(buckets: Vec<StoredCodexTurnStateBucket>) -> Self {
        Self {
            buckets: RwLock::new(
                buckets
                    .into_iter()
                    .map(|bucket| ((bucket.key_id.clone(), bucket.model.clone()), bucket))
                    .collect(),
            ),
        }
    }
}

#[async_trait]
impl CodexTurnStateBucketReadRepository for InMemoryCodexTurnStateBucketRepository {
    async fn get_bucket(
        &self,
        key_id: &str,
        model: &str,
    ) -> Result<Option<StoredCodexTurnStateBucket>, DataLayerError> {
        Ok(self
            .buckets
            .read()
            .map_err(|_| {
                DataLayerError::UnexpectedValue("codex turn-state memory lock poisoned".into())
            })?
            .get(&(key_id.to_string(), model.to_string()))
            .cloned())
    }

    async fn list_buckets(&self) -> Result<Vec<StoredCodexTurnStateBucket>, DataLayerError> {
        Ok(self
            .buckets
            .read()
            .map_err(|_| {
                DataLayerError::UnexpectedValue("codex turn-state memory lock poisoned".into())
            })?
            .values()
            .cloned()
            .collect())
    }

    async fn buckets_due_for_renewal(
        &self,
        threshold_unix_secs: i64,
    ) -> Result<Vec<StoredCodexTurnStateBucket>, DataLayerError> {
        Ok(self
            .buckets
            .read()
            .map_err(|_| {
                DataLayerError::UnexpectedValue("codex turn-state memory lock poisoned".into())
            })?
            .values()
            .filter(|bucket| bucket.expires_at_unix_secs <= threshold_unix_secs)
            .cloned()
            .collect())
    }
}

#[async_trait]
impl CodexTurnStateBucketWriteRepository for InMemoryCodexTurnStateBucketRepository {
    async fn upsert_bucket(
        &self,
        input: UpsertCodexTurnStateBucket,
    ) -> Result<bool, DataLayerError> {
        let mut buckets = self.buckets.write().map_err(|_| {
            DataLayerError::UnexpectedValue("codex turn-state memory lock poisoned".into())
        })?;
        let identity = (input.key_id.clone(), input.model.clone());
        if buckets
            .get(&identity)
            .is_some_and(|existing| existing.issued_at_unix_secs >= input.issued_at_unix_secs)
        {
            return Ok(false);
        }
        buckets.insert(
            identity,
            StoredCodexTurnStateBucket {
                key_id: input.key_id,
                model: input.model,
                encrypted_value: input.encrypted_value,
                value_len: input.value_len,
                issued_at_unix_secs: input.issued_at_unix_secs,
                harvested_at_unix_secs: input.harvested_at_unix_secs,
                expires_at_unix_secs: input.expires_at_unix_secs,
                source: input.source,
                last_exit: input.last_exit,
            },
        );
        Ok(true)
    }

    async fn delete_buckets_for_key(&self, key_id: &str) -> Result<u64, DataLayerError> {
        let mut buckets = self.buckets.write().map_err(|_| {
            DataLayerError::UnexpectedValue("codex turn-state memory lock poisoned".into())
        })?;
        let before = buckets.len();
        buckets.retain(|(stored_key_id, _), _| stored_key_id != key_id);
        Ok((before.saturating_sub(buckets.len())) as u64)
    }

    async fn clear_all_buckets(&self) -> Result<u64, DataLayerError> {
        let mut buckets = self.buckets.write().map_err(|_| {
            DataLayerError::UnexpectedValue("codex turn-state memory lock poisoned".into())
        })?;
        let count = buckets.len() as u64;
        buckets.clear();
        Ok(count)
    }
}
