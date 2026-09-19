use async_trait::async_trait;
use sqlx::{PgPool, Row};

use aether_data_contracts::repository::codex_turn_state::{
    CodexTurnStateBucketReadRepository, CodexTurnStateBucketWriteRepository, CodexTurnStateSource,
    StoredCodexTurnStateBucket, UpsertCodexTurnStateBucket,
};
use aether_data_contracts::DataLayerError;

use crate::error::SqlxResultExt;

#[derive(Debug, Clone)]
pub struct SqlxCodexTurnStateBucketRepository {
    pool: PgPool,
}

impl SqlxCodexTurnStateBucketRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn map_row(row: &sqlx::postgres::PgRow) -> Result<StoredCodexTurnStateBucket, DataLayerError> {
    let source_value: String = row.try_get("source").map_postgres_err()?;
    let source = CodexTurnStateSource::parse(&source_value).ok_or_else(|| {
        DataLayerError::UnexpectedValue(format!(
            "unknown codex turn-state bucket source: {source_value}"
        ))
    })?;
    Ok(StoredCodexTurnStateBucket {
        key_id: row.try_get("key_id").map_postgres_err()?,
        model: row.try_get("model").map_postgres_err()?,
        encrypted_value: row.try_get("encrypted_value").map_postgres_err()?,
        value_len: row.try_get("value_len").map_postgres_err()?,
        issued_at_unix_secs: row.try_get("issued_at_unix_secs").map_postgres_err()?,
        harvested_at_unix_secs: row.try_get("harvested_at_unix_secs").map_postgres_err()?,
        expires_at_unix_secs: row.try_get("expires_at_unix_secs").map_postgres_err()?,
        source,
        last_exit: row.try_get("last_exit").map_postgres_err()?,
    })
}

const SELECT_COLUMNS: &str = r#"
SELECT
  key_id,
  model,
  encrypted_value,
  value_len,
  EXTRACT(EPOCH FROM issued_at)::BIGINT AS issued_at_unix_secs,
  EXTRACT(EPOCH FROM harvested_at)::BIGINT AS harvested_at_unix_secs,
  EXTRACT(EPOCH FROM expires_at)::BIGINT AS expires_at_unix_secs,
  source,
  last_exit
FROM codex_turn_state_buckets
"#;

#[async_trait]
impl CodexTurnStateBucketReadRepository for SqlxCodexTurnStateBucketRepository {
    async fn get_bucket(
        &self,
        key_id: &str,
        model: &str,
    ) -> Result<Option<StoredCodexTurnStateBucket>, DataLayerError> {
        let row = sqlx::query(&format!(
            "{SELECT_COLUMNS} WHERE key_id = $1 AND model = $2 LIMIT 1"
        ))
        .bind(key_id)
        .bind(model)
        .fetch_optional(&self.pool)
        .await
        .map_postgres_err()?;
        row.as_ref().map(map_row).transpose()
    }

    async fn list_buckets(&self) -> Result<Vec<StoredCodexTurnStateBucket>, DataLayerError> {
        sqlx::query(&format!("{SELECT_COLUMNS} ORDER BY key_id, model"))
            .fetch_all(&self.pool)
            .await
            .map_postgres_err()?
            .iter()
            .map(map_row)
            .collect()
    }

    async fn buckets_due_for_renewal(
        &self,
        threshold_unix_secs: i64,
    ) -> Result<Vec<StoredCodexTurnStateBucket>, DataLayerError> {
        sqlx::query(&format!(
            "{SELECT_COLUMNS} WHERE expires_at <= to_timestamp($1::double precision) ORDER BY expires_at"
        ))
        .bind(threshold_unix_secs)
        .fetch_all(&self.pool)
        .await
        .map_postgres_err()?
        .iter()
        .map(map_row)
        .collect()
    }
}

#[async_trait]
impl CodexTurnStateBucketWriteRepository for SqlxCodexTurnStateBucketRepository {
    async fn upsert_bucket(
        &self,
        input: UpsertCodexTurnStateBucket,
    ) -> Result<bool, DataLayerError> {
        if input.key_id.trim().is_empty() || input.model.trim().is_empty() {
            return Err(DataLayerError::InvalidInput(
                "codex turn-state bucket identity is empty".to_string(),
            ));
        }
        let result = sqlx::query(
            r#"
INSERT INTO codex_turn_state_buckets (
  key_id, model, encrypted_value, value_len,
  issued_at, harvested_at, expires_at, source, last_exit
)
VALUES (
  $1, $2, $3, $4,
  to_timestamp($5::double precision),
  to_timestamp($6::double precision),
  to_timestamp($7::double precision),
  $8, $9
)
ON CONFLICT (key_id, model) DO UPDATE SET
  encrypted_value = EXCLUDED.encrypted_value,
  value_len = EXCLUDED.value_len,
  issued_at = EXCLUDED.issued_at,
  harvested_at = EXCLUDED.harvested_at,
  expires_at = EXCLUDED.expires_at,
  source = EXCLUDED.source,
  last_exit = EXCLUDED.last_exit
WHERE codex_turn_state_buckets.issued_at < EXCLUDED.issued_at
"#,
        )
        .bind(input.key_id)
        .bind(input.model)
        .bind(input.encrypted_value)
        .bind(input.value_len)
        .bind(input.issued_at_unix_secs)
        .bind(input.harvested_at_unix_secs)
        .bind(input.expires_at_unix_secs)
        .bind(input.source.as_str())
        .bind(input.last_exit)
        .execute(&self.pool)
        .await
        .map_postgres_err()?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete_buckets_for_key(&self, key_id: &str) -> Result<u64, DataLayerError> {
        Ok(
            sqlx::query("DELETE FROM codex_turn_state_buckets WHERE key_id = $1")
                .bind(key_id)
                .execute(&self.pool)
                .await
                .map_postgres_err()?
                .rows_affected(),
        )
    }

    async fn clear_all_buckets(&self) -> Result<u64, DataLayerError> {
        Ok(sqlx::query("DELETE FROM codex_turn_state_buckets")
            .execute(&self.pool)
            .await
            .map_postgres_err()?
            .rows_affected())
    }
}
