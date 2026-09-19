use aether_data::DataLayerError;
use aether_data_contracts::repository::codex_turn_state::{
    StoredCodexTurnStateBucket, UpsertCodexTurnStateBucket,
};

use super::GatewayDataState;

impl GatewayDataState {
    pub(crate) fn has_codex_turn_state_bucket_reader(&self) -> bool {
        self.codex_turn_state_bucket_reader.is_some()
    }

    pub(crate) fn has_codex_turn_state_bucket_writer(&self) -> bool {
        self.codex_turn_state_bucket_writer.is_some()
    }

    /// `None` means this data state has no configured bucket repository. An
    /// empty `Some(Vec::new())` is a real, authoritative empty database result.
    pub(crate) async fn list_codex_turn_state_buckets(
        &self,
    ) -> Result<Option<Vec<StoredCodexTurnStateBucket>>, DataLayerError> {
        match &self.codex_turn_state_bucket_reader {
            Some(repository) => repository.list_buckets().await.map(Some),
            None => Ok(None),
        }
    }

    pub(crate) async fn upsert_codex_turn_state_bucket(
        &self,
        input: UpsertCodexTurnStateBucket,
    ) -> Result<Option<bool>, DataLayerError> {
        match &self.codex_turn_state_bucket_writer {
            Some(repository) => repository.upsert_bucket(input).await.map(Some),
            None => Ok(None),
        }
    }

    pub(crate) async fn delete_codex_turn_state_buckets_for_key(
        &self,
        key_id: &str,
    ) -> Result<Option<u64>, DataLayerError> {
        match &self.codex_turn_state_bucket_writer {
            Some(repository) => repository.delete_buckets_for_key(key_id).await.map(Some),
            None => Ok(None),
        }
    }

    pub(crate) async fn clear_codex_turn_state_buckets(
        &self,
    ) -> Result<Option<u64>, DataLayerError> {
        match &self.codex_turn_state_bucket_writer {
            Some(repository) => repository.clear_all_buckets().await.map(Some),
            None => Ok(None),
        }
    }
}
