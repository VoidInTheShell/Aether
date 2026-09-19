use async_trait::async_trait;

/// The only two sources that are allowed to create a persisted bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexTurnStateSource {
    Probe,
    Passive,
}

impl CodexTurnStateSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Passive => "passive",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "probe" => Some(Self::Probe),
            "passive" => Some(Self::Passive),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCodexTurnStateBucket {
    pub key_id: String,
    pub model: String,
    pub encrypted_value: Vec<u8>,
    pub value_len: i32,
    pub issued_at_unix_secs: i64,
    pub harvested_at_unix_secs: i64,
    pub expires_at_unix_secs: i64,
    pub source: CodexTurnStateSource,
    pub last_exit: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertCodexTurnStateBucket {
    pub key_id: String,
    pub model: String,
    pub encrypted_value: Vec<u8>,
    pub value_len: i32,
    pub issued_at_unix_secs: i64,
    pub harvested_at_unix_secs: i64,
    pub expires_at_unix_secs: i64,
    pub source: CodexTurnStateSource,
    pub last_exit: String,
}

#[async_trait]
pub trait CodexTurnStateBucketReadRepository: Send + Sync {
    async fn get_bucket(
        &self,
        key_id: &str,
        model: &str,
    ) -> Result<Option<StoredCodexTurnStateBucket>, crate::DataLayerError>;

    async fn list_buckets(&self) -> Result<Vec<StoredCodexTurnStateBucket>, crate::DataLayerError>;

    async fn buckets_due_for_renewal(
        &self,
        threshold_unix_secs: i64,
    ) -> Result<Vec<StoredCodexTurnStateBucket>, crate::DataLayerError>;
}

#[async_trait]
pub trait CodexTurnStateBucketWriteRepository: Send + Sync {
    /// Implementations must only replace an existing row when the incoming
    /// Fernet issue time is newer than the stored one.
    async fn upsert_bucket(
        &self,
        input: UpsertCodexTurnStateBucket,
    ) -> Result<bool, crate::DataLayerError>;

    async fn delete_buckets_for_key(&self, key_id: &str) -> Result<u64, crate::DataLayerError>;

    async fn clear_all_buckets(&self) -> Result<u64, crate::DataLayerError>;
}
