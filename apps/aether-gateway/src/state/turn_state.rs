use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aether_contracts::ExecutionPlan;
use aether_crypto::{decrypt_python_fernet_ciphertext, encrypt_python_fernet_plaintext};
use aether_data_contracts::repository::codex_turn_state::{
    CodexTurnStateSource, StoredCodexTurnStateBucket, UpsertCodexTurnStateBucket,
};
use aether_data_contracts::repository::provider_catalog::ProviderCatalogKeyHealthStateUpdate;
use aether_turn_state::{
    decide_header, issued_at_unix_secs, template_usable, AccountVerdict, AccountVerdictState,
    DecisionAction, InjectMode, LiveTemplate, TurnStateDecisionInput, VerdictTransition,
    DEFAULT_REPLACE_LENGTH, DEFAULT_TEMPLATE_LENGTH, DEFAULT_TTL_SECONDS,
};
use axum::http;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use url::Url;

use super::AppState;
use crate::clock::current_unix_secs;
use crate::orchestration::{
    project_local_degraded_health, project_local_failure_health, project_local_key_circuit_closed,
    project_local_key_circuit_open, project_local_success_health, LocalFailoverClassification,
};
use crate::GatewayError;

pub(crate) const TURN_STATE_CONFIG_KEY: &str = "module.codex_turn_state.config";
pub(crate) const TURN_STATE_SCOPE_KEY: &str = "module.codex_turn_state.scope";
const TURN_STATE_RUNTIME_KEY: &str = "module.codex_turn_state.runtime";
const TURN_STATE_DESCRIPTION: &str = "Codex Turn-State runtime state (encrypted bucket values)";
const TURN_STATE_PROBE_LOCK_KEY: &str = "codex_turn_state:probe";
const TURN_STATE_PROBE_LOCK_TTL: Duration = Duration::from_secs(60 * 60);
const TURN_STATE_CONFIG_FIELDS: &[(&str, &str)] = &[
    ("inject_mode", "module.codex_turn_state.inject_mode"),
    ("harvest_inband", "module.codex_turn_state.harvest_inband"),
    ("degrade_action", "module.codex_turn_state.degrade_action"),
    (
        "degrade_threshold",
        "module.codex_turn_state.degrade_threshold",
    ),
    ("ttl_seconds", "module.codex_turn_state.ttl_seconds"),
    ("template_length", "module.codex_turn_state.template_length"),
    ("replace_length", "module.codex_turn_state.replace_length"),
    ("auto_renew", "module.codex_turn_state.auto_renew"),
    (
        "renew_threshold_seconds",
        "module.codex_turn_state.renew_threshold_seconds",
    ),
    (
        "account_backoff_seconds",
        "module.codex_turn_state.account_backoff_seconds",
    ),
    (
        "exit_cooldown_seconds",
        "module.codex_turn_state.exit_cooldown_seconds",
    ),
    (
        "rotating_max_attempts",
        "module.codex_turn_state.rotating_max_attempts",
    ),
    (
        "max_accounts_in_flight",
        "module.codex_turn_state.max_accounts_in_flight",
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct TurnStateConfig {
    /// Module toggles are canonical system-config keys owned by the module
    /// registry.  They are deliberately omitted from the composite config
    /// payload so the frozen frontend cannot accidentally reset them while
    /// saving one of the policy fields below.
    #[serde(skip)]
    pub enabled: bool,
    #[serde(skip)]
    pub dry_run: bool,
    pub inject_mode: InjectMode,
    pub harvest_inband: bool,
    pub degrade_action: String,
    pub degrade_threshold: u32,
    pub ttl_seconds: u64,
    pub template_length: usize,
    pub replace_length: usize,
    pub auto_renew: bool,
    pub renew_threshold_seconds: u64,
    pub account_backoff_seconds: u64,
    pub exit_cooldown_seconds: u64,
    pub rotating_max_attempts: u32,
    pub max_accounts_in_flight: u32,
}

impl Default for TurnStateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dry_run: false,
            inject_mode: InjectMode::ReplaceOnly,
            harvest_inband: true,
            degrade_action: "none".to_string(),
            degrade_threshold: 3,
            ttl_seconds: DEFAULT_TTL_SECONDS,
            template_length: DEFAULT_TEMPLATE_LENGTH,
            replace_length: DEFAULT_REPLACE_LENGTH,
            auto_renew: true,
            renew_threshold_seconds: 300,
            account_backoff_seconds: 600,
            exit_cooldown_seconds: 3300,
            rotating_max_attempts: 10,
            max_accounts_in_flight: 4,
        }
    }
}

impl TurnStateConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.degrade_action.as_str(),
            "none" | "downweight" | "disable"
        ) {
            return Err("degrade_action must be none, downweight, or disable".to_string());
        }
        if !(1..=10).contains(&self.degrade_threshold) {
            return Err("degrade_threshold must be between 1 and 10".to_string());
        }
        if !(60..=86_400).contains(&self.ttl_seconds) {
            return Err("ttl_seconds must be between 60 and 86400".to_string());
        }
        if !(1..=8192).contains(&self.template_length) || !(1..=8192).contains(&self.replace_length)
        {
            return Err(
                "template_length and replace_length must be between 1 and 8192".to_string(),
            );
        }
        if self.renew_threshold_seconds > 3600 {
            return Err("renew_threshold_seconds must be between 0 and 3600".to_string());
        }
        if !(30..=86_400).contains(&self.account_backoff_seconds)
            || !(60..=86_400).contains(&self.exit_cooldown_seconds)
        {
            return Err("probe cooldowns are outside the allowed range".to_string());
        }
        if !(1..=100).contains(&self.rotating_max_attempts)
            || !(1..=32).contains(&self.max_accounts_in_flight)
        {
            return Err("probe concurrency settings are outside the allowed range".to_string());
        }
        Ok(())
    }

    fn fingerprint(&self) -> (u64, usize, usize) {
        (self.ttl_seconds, self.template_length, self.replace_length)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct TurnStateScope {
    pub key_ids: Vec<String>,
    pub models: Vec<String>,
    pub probe_proxies: Vec<String>,
    pub probe_proxies_rotating: Vec<String>,
}

impl TurnStateScope {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for model in &self.models {
            if model.trim().is_empty() || !model.contains('-') {
                return Err(format!("model must contain a hyphen: {model}"));
            }
        }
        for proxy in self
            .probe_proxies
            .iter()
            .chain(self.probe_proxies_rotating.iter())
        {
            let parsed = Url::parse(proxy).map_err(|_| "proxy URL is invalid".to_string())?;
            if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h")
                || parsed.host_str().is_none()
                || parsed.port().is_none()
            {
                return Err("proxy URL must include a supported scheme, host, and port".to_string());
            }
        }
        Ok(())
    }

    pub(crate) fn masked(&self) -> Self {
        Self {
            key_ids: self.key_ids.clone(),
            models: self.models.clone(),
            probe_proxies: self
                .probe_proxies
                .iter()
                .map(|value| mask_proxy(value))
                .collect(),
            probe_proxies_rotating: self
                .probe_proxies_rotating
                .iter()
                .map(|value| mask_proxy(value))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Counters {
    harvest: u64,
    substitute: u64,
    inject: u64,
    pass: u64,
    skip: u64,
    since_unix: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct ProbeRun {
    running: bool,
    started_at_unix: Option<u64>,
    finished_at_unix: Option<u64>,
    total: usize,
    done: usize,
    lines: Vec<String>,
}

#[derive(Clone)]
struct BucketState {
    value: String,
    issued_at_unix: u64,
    expires_at_unix: u64,
    source: String,
    last_exit: Option<String>,
}

impl fmt::Debug for BucketState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BucketState")
            .field("value_len", &self.value.len())
            .field("issued_at_unix", &self.issued_at_unix)
            .field("expires_at_unix", &self.expires_at_unix)
            .field("source", &self.source)
            .field("last_exit", &self.last_exit)
            .finish()
    }
}

#[derive(Clone, Default)]
struct RuntimeMemory {
    buckets: BTreeMap<String, BucketState>,
    accounts: BTreeMap<String, AccountVerdictState>,
    /// The health action that was applied when an account entered degraded
    /// state.  Keeping this separate from the current config makes recovery
    /// symmetric even when an operator changes `degrade_action` to `none`.
    health_action_applied: BTreeMap<String, String>,
    account_backoff_until: BTreeMap<String, u64>,
    exit_cooldowns_until: BTreeMap<String, u64>,
    rotating_cooldowns_until: BTreeMap<String, u64>,
    counters: Counters,
    probe_run: ProbeRun,
}

impl fmt::Debug for RuntimeMemory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeMemory")
            .field("bucket_count", &self.buckets.len())
            .field("account_count", &self.accounts.len())
            .field("counters", &self.counters)
            .field("probe_run", &self.probe_run)
            .finish()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedRuntime {
    buckets: Vec<PersistedBucket>,
    accounts: BTreeMap<String, AccountVerdictState>,
    #[serde(default)]
    health_action_applied: BTreeMap<String, String>,
    #[serde(default)]
    account_backoff_until: BTreeMap<String, u64>,
    #[serde(default)]
    exit_cooldowns_until: BTreeMap<String, u64>,
    #[serde(default)]
    rotating_cooldowns_until: BTreeMap<String, u64>,
    counters: Counters,
    probe_run: ProbeRun,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedBucket {
    key_id: String,
    model: String,
    encrypted_value: String,
    issued_at_unix: u64,
    expires_at_unix: u64,
    source: String,
    last_exit: Option<String>,
}

enum ProbeOneOutcome {
    Template {
        value: String,
        issued_at_unix: u64,
        exit: Option<String>,
    },
    Degraded {
        exit: Option<String>,
    },
    AccountLimited(u16),
    Skipped(String),
}

enum ProbeResponse {
    Template { value: String, issued_at_unix: u64 },
    Degraded,
    AccountLimited(u16),
    NoTurnState,
    UnknownLength(usize),
    UpstreamFailure(u16),
    NetworkFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnStateHarvestResult {
    Disabled,
    Ignored,
    Stored,
    Older,
}

pub(crate) struct TurnStateRuntime {
    loaded: AtomicBool,
    renewal_worker_started: AtomicBool,
    persist_lock: tokio::sync::Mutex<()>,
    memory: RwLock<RuntimeMemory>,
}

impl fmt::Debug for TurnStateRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TurnStateRuntime")
            .field("loaded", &self.loaded.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl AppState {
    pub(crate) async fn read_codex_turn_state_buckets(
        &self,
    ) -> Result<Option<Vec<StoredCodexTurnStateBucket>>, GatewayError> {
        self.data
            .list_codex_turn_state_buckets()
            .await
            .map_err(|err| GatewayError::Internal(format!("turn-state bucket load failed: {err}")))
    }

    pub(crate) async fn upsert_codex_turn_state_bucket(
        &self,
        input: UpsertCodexTurnStateBucket,
    ) -> Result<Option<bool>, GatewayError> {
        self.data
            .upsert_codex_turn_state_bucket(input)
            .await
            .map_err(|err| GatewayError::Internal(format!("turn-state bucket write failed: {err}")))
    }

    pub(crate) async fn clear_codex_turn_state_buckets(&self) -> Result<Option<u64>, GatewayError> {
        self.data
            .clear_codex_turn_state_buckets()
            .await
            .map_err(|err| GatewayError::Internal(format!("turn-state bucket clear failed: {err}")))
    }
}

impl Default for TurnStateRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnStateRuntime {
    pub(crate) fn new() -> Self {
        Self {
            loaded: AtomicBool::new(false),
            renewal_worker_started: AtomicBool::new(false),
            persist_lock: tokio::sync::Mutex::new(()),
            memory: RwLock::new(RuntimeMemory::default()),
        }
    }

    /// Register the fixed-interval renewal loop once per process. The shared
    /// singleton-worker lease ensures only one gateway instance performs a walk
    /// when Redis-backed runtime state is available.
    pub(crate) fn spawn_renewal_worker(&self, app: AppState) {
        if self
            .renewal_worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let runtime = Arc::clone(&app.codex_turn_state);
        std::mem::drop(crate::task_runtime::spawn_singleton_worker(
            app,
            crate::task_runtime::TASK_KEY_CODEX_TURN_STATE,
            move |app| {
                let runtime = Arc::clone(&runtime);
                async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        let Ok(config) = runtime.config(&app).await else {
                            continue;
                        };
                        if !config.enabled || !config.auto_renew {
                            continue;
                        }
                        let now = current_unix_secs();
                        let due_keys = {
                            let memory = runtime.memory.read().await;
                            memory
                                .buckets
                                .iter()
                                .filter_map(|(key, bucket)| {
                                    let due = bucket.expires_at_unix
                                        <= now.saturating_add(config.renew_threshold_seconds);
                                    due.then(|| {
                                        key.split_once('\0').map(|(key_id, _)| key_id.to_string())
                                    })
                                    .flatten()
                                })
                                .collect::<std::collections::BTreeSet<_>>()
                                .into_iter()
                                .collect::<Vec<_>>()
                        };
                        if due_keys.is_empty() || runtime.probe_is_running().await {
                            continue;
                        }
                        let Ok(run) = runtime.start_probe(&app, Some(due_keys.clone())).await
                        else {
                            continue;
                        };
                        if run.get("running").and_then(Value::as_bool) != Some(true) {
                            continue;
                        }
                        Arc::clone(&runtime)
                            .run_probe(app.clone(), Some(due_keys))
                            .await;
                    }
                }
            },
        ));
    }

    async fn ensure_loaded(&self, app: &AppState) -> Result<(), GatewayError> {
        if self.loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        let persisted = app
            .read_system_config_json_value(TURN_STATE_RUNTIME_KEY)
            .await?;
        let mut memory = RuntimeMemory::default();
        let mut legacy_buckets = BTreeMap::new();
        if let Some(value) = persisted {
            if let Ok(snapshot) = serde_json::from_value::<PersistedRuntime>(value) {
                memory.accounts = snapshot.accounts;
                memory.health_action_applied = snapshot.health_action_applied;
                memory.account_backoff_until = snapshot.account_backoff_until;
                memory.exit_cooldowns_until = snapshot.exit_cooldowns_until;
                memory.rotating_cooldowns_until = snapshot.rotating_cooldowns_until;
                memory.counters = snapshot.counters;
                memory.probe_run = snapshot.probe_run;
                if let Some(secret) = app.data.encryption_key() {
                    for bucket in snapshot.buckets {
                        let Ok(value) =
                            decrypt_python_fernet_ciphertext(secret, &bucket.encrypted_value)
                        else {
                            continue;
                        };
                        let key = bucket_key(&bucket.key_id, &bucket.model);
                        memory.buckets.insert(
                            key,
                            BucketState {
                                value,
                                issued_at_unix: bucket.issued_at_unix,
                                expires_at_unix: bucket.expires_at_unix,
                                source: bucket.source,
                                last_exit: bucket.last_exit,
                            },
                        );
                    }
                }
            }
        }
        // The bucket table is the authoritative store whenever the configured
        // data layer exposes it.  Keep the old encrypted system-config format
        // only as a one-time migration source for installations upgraded from
        // the pre-table implementation.
        let mut migrate_legacy_buckets = false;
        if let Some(repository_buckets) = app.read_codex_turn_state_buckets().await? {
            legacy_buckets = memory.buckets.clone();
            memory.buckets.clear();
            if repository_buckets.is_empty() && !legacy_buckets.is_empty() {
                memory.buckets = legacy_buckets.clone();
                migrate_legacy_buckets = true;
            } else if let Some(secret) = app.data.encryption_key() {
                for bucket in repository_buckets {
                    let Ok(ciphertext) = String::from_utf8(bucket.encrypted_value) else {
                        continue;
                    };
                    let Ok(value) = decrypt_python_fernet_ciphertext(secret, &ciphertext) else {
                        continue;
                    };
                    let key = bucket_key(&bucket.key_id, &bucket.model);
                    memory.buckets.insert(
                        key,
                        BucketState {
                            value,
                            issued_at_unix: bucket.issued_at_unix_secs.max(0) as u64,
                            expires_at_unix: bucket.expires_at_unix_secs.max(0) as u64,
                            source: bucket.source.as_str().to_string(),
                            last_exit: (!bucket.last_exit.is_empty()).then_some(bucket.last_exit),
                        },
                    );
                }
            }
        }
        *self.memory.write().await = memory;
        self.loaded.store(true, Ordering::Release);
        if migrate_legacy_buckets {
            self.persist(app).await?;
        }
        Ok(())
    }

    async fn persist(&self, app: &AppState) -> Result<(), GatewayError> {
        // Probe progress can be persisted by several account workers at once.
        // Serialize the snapshot/write pair so a slower database write cannot
        // overwrite a newer in-memory snapshot with an older one.
        let _persist_guard = self.persist_lock.lock().await;
        let Some(secret) = app.data.encryption_key() else {
            // A missing application encryption key is already a fail-closed
            // state for credential storage. Never fall back to plaintext here.
            return Ok(());
        };
        let memory = self.memory.read().await.clone();
        let repository_writer = app.data.has_codex_turn_state_bucket_writer();
        let mut persisted_buckets = Vec::with_capacity(memory.buckets.len());
        let mut repository_buckets = Vec::with_capacity(memory.buckets.len());
        for (key, bucket) in memory.buckets {
            let Some((key_id, model)) = key.split_once('\0') else {
                continue;
            };
            let encrypted_value =
                encrypt_python_fernet_plaintext(secret, &bucket.value).map_err(|err| {
                    GatewayError::Internal(format!("turn-state encryption failed: {err}"))
                })?;
            if repository_writer {
                let Some(source) = CodexTurnStateSource::parse(&bucket.source) else {
                    continue;
                };
                repository_buckets.push(UpsertCodexTurnStateBucket {
                    key_id: key_id.to_string(),
                    model: model.to_string(),
                    value_len: i32::try_from(bucket.value.len()).unwrap_or(i32::MAX),
                    issued_at_unix_secs: i64::try_from(bucket.issued_at_unix).unwrap_or(i64::MAX),
                    harvested_at_unix_secs: i64::try_from(current_unix_secs()).unwrap_or(i64::MAX),
                    expires_at_unix_secs: i64::try_from(bucket.expires_at_unix).unwrap_or(i64::MAX),
                    source,
                    last_exit: bucket.last_exit.unwrap_or_default(),
                    encrypted_value: encrypted_value.into_bytes(),
                });
            } else {
                persisted_buckets.push(PersistedBucket {
                    key_id: key_id.to_string(),
                    model: model.to_string(),
                    encrypted_value,
                    issued_at_unix: bucket.issued_at_unix,
                    expires_at_unix: bucket.expires_at_unix,
                    source: bucket.source,
                    last_exit: bucket.last_exit,
                });
            }
        }
        if repository_writer {
            for bucket in repository_buckets {
                app.upsert_codex_turn_state_bucket(bucket).await?;
            }
        }
        let persisted = PersistedRuntime {
            // With a repository-backed deployment, keep credentials out of the
            // generic system-config document.  The fallback document remains
            // encrypted for installations without the migration/table.
            buckets: persisted_buckets,
            accounts: memory.accounts,
            health_action_applied: memory.health_action_applied,
            account_backoff_until: memory.account_backoff_until,
            exit_cooldowns_until: memory.exit_cooldowns_until,
            rotating_cooldowns_until: memory.rotating_cooldowns_until,
            counters: memory.counters,
            probe_run: memory.probe_run,
        };
        let value = serde_json::to_value(persisted).map_err(|err| {
            GatewayError::Internal(format!("turn-state state encode failed: {err}"))
        })?;
        app.upsert_system_config_json_value(
            TURN_STATE_RUNTIME_KEY,
            &value,
            Some(TURN_STATE_DESCRIPTION),
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn config(&self, app: &AppState) -> Result<TurnStateConfig, GatewayError> {
        // Keep the composite module document for the module API, but overlay
        // the canonical system-config keys.  The generic admin settings page
        // writes those individual keys, so reading only the composite object
        // would otherwise leave the runtime with stale values.
        let mut merged = app
            .read_system_config_json_value(TURN_STATE_CONFIG_KEY)
            .await?
            .filter(Value::is_object)
            .unwrap_or_else(|| {
                serde_json::to_value(TurnStateConfig::default()).unwrap_or_else(|_| json!({}))
            });
        let Some(object) = merged.as_object_mut() else {
            return Ok(TurnStateConfig::default());
        };
        for (field, key) in TURN_STATE_CONFIG_FIELDS {
            if let Some(value) = app.read_system_config_json_value(key).await? {
                object.insert((*field).to_string(), value);
            }
        }
        let mut config: TurnStateConfig = serde_json::from_value(merged).unwrap_or_default();
        config.enabled = app
            .read_system_config_json_value("module.codex_turn_state.enabled")
            .await?
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        config.dry_run = app
            .read_system_config_json_value("module.codex_turn_state.dry_run")
            .await?
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        Ok(config)
    }

    async fn scope_raw(&self, app: &AppState) -> Result<TurnStateScope, GatewayError> {
        let value = app
            .read_system_config_json_value(TURN_STATE_SCOPE_KEY)
            .await?;
        Ok(value
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default())
    }

    pub(crate) async fn scope(&self, app: &AppState) -> Result<TurnStateScope, GatewayError> {
        Ok(self.scope_raw(app).await?.masked())
    }

    pub(crate) async fn update_scope(
        &self,
        app: &AppState,
        mut scope: TurnStateScope,
    ) -> Result<TurnStateScope, GatewayError> {
        let old_scope = self.scope_raw(app).await?;
        merge_masked_proxy_values(&mut scope.probe_proxies, &old_scope.probe_proxies);
        merge_masked_proxy_values(
            &mut scope.probe_proxies_rotating,
            &old_scope.probe_proxies_rotating,
        );
        scope
            .validate()
            .map_err(|err| GatewayError::Internal(err))?;
        if !scope.key_ids.is_empty() {
            let requested = scope
                .key_ids
                .iter()
                .map(|key_id| key_id.trim())
                .filter(|key_id| !key_id.is_empty())
                .map(ToOwned::to_owned)
                .collect::<std::collections::BTreeSet<_>>();
            let known = app
                .read_provider_catalog_keys_by_ids(&requested.iter().cloned().collect::<Vec<_>>())
                .await?
                .into_iter()
                .map(|key| key.id)
                .collect::<std::collections::BTreeSet<_>>();
            if let Some(missing) = requested.iter().find(|key_id| !known.contains(*key_id)) {
                return Err(GatewayError::Client {
                    status: http::StatusCode::BAD_REQUEST,
                    message: format!("探测范围包含不存在的号池 Key: {missing}"),
                });
            }
            scope.key_ids = requested.into_iter().collect();
        }
        let value = serde_json::to_value(&scope).map_err(|err| {
            GatewayError::Internal(format!("turn-state scope encode failed: {err}"))
        })?;
        app.upsert_system_config_json_value(TURN_STATE_SCOPE_KEY, &value, None)
            .await?;
        Ok(scope.masked())
    }

    pub(crate) async fn update_config(
        &self,
        app: &AppState,
        config: TurnStateConfig,
    ) -> Result<TurnStateConfig, GatewayError> {
        config
            .validate()
            .map_err(|err| GatewayError::Internal(err))?;
        self.ensure_loaded(app).await?;
        let old = self.config(app).await?;
        let mut config = config;
        // `enabled` and `dry_run` are not part of the frozen frontend's
        // config DTO.  Preserve their canonical values when a policy save
        // arrives instead of interpreting omitted fields as `false`.
        config.enabled = old.enabled;
        config.dry_run = old.dry_run;
        if old.fingerprint() != config.fingerprint() {
            let mut memory = self.memory.write().await;
            memory.buckets.clear();
            memory.counters = Counters {
                since_unix: current_unix_secs(),
                ..Counters::default()
            };
            drop(memory);
            app.clear_codex_turn_state_buckets().await?;
        }
        let value = serde_json::to_value(&config).map_err(|err| {
            GatewayError::Internal(format!("turn-state config encode failed: {err}"))
        })?;
        app.upsert_system_config_json_value(TURN_STATE_CONFIG_KEY, &value, None)
            .await?;
        for (field, key) in TURN_STATE_CONFIG_FIELDS {
            if let Some(field_value) = value.get(*field) {
                app.upsert_system_config_json_value(key, field_value, None)
                    .await?;
            }
        }
        self.persist(app).await?;
        Ok(config)
    }

    pub(crate) async fn set_dry_run(
        &self,
        app: &AppState,
        dry_run: bool,
    ) -> Result<(), GatewayError> {
        let value = Value::Bool(dry_run);
        app.upsert_system_config_json_value("module.codex_turn_state.dry_run", &value, None)
            .await
            .map(|_| ())
    }

    pub(crate) async fn clear(&self, app: &AppState) -> Result<usize, GatewayError> {
        self.ensure_loaded(app).await?;
        let mut memory = self.memory.write().await;
        let cleared = memory.buckets.len();
        memory.buckets.clear();
        memory.counters = Counters {
            since_unix: current_unix_secs(),
            ..Counters::default()
        };
        drop(memory);
        app.clear_codex_turn_state_buckets().await?;
        self.persist(app).await?;
        Ok(cleared)
    }

    pub(crate) async fn start_probe(
        &self,
        app: &AppState,
        requested_key_ids: Option<Vec<String>>,
    ) -> Result<Value, GatewayError> {
        self.ensure_loaded(app).await?;
        let scope = self.scope_raw(app).await?;
        let key_ids = requested_key_ids
            .unwrap_or_else(|| scope.key_ids.clone())
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        let total = key_ids.len().saturating_mul(scope.models.len());
        {
            let mut memory = self.memory.write().await;
            if memory.probe_run.running {
                return Ok(serde_json::to_value(&memory.probe_run).unwrap_or_else(|_| json!({})));
            }
            memory.probe_run = ProbeRun {
                running: true,
                started_at_unix: Some(current_unix_secs()),
                finished_at_unix: None,
                total,
                done: 0,
                lines: Vec::new(),
            };
        }
        self.persist(app).await?;
        Ok(self.probe_run_json().await)
    }

    pub(crate) async fn cancel_probe(&self, app: &AppState) -> Result<Value, GatewayError> {
        self.ensure_loaded(app).await?;
        {
            let mut memory = self.memory.write().await;
            memory.probe_run.running = false;
            memory.probe_run.finished_at_unix = Some(current_unix_secs());
            memory.probe_run.lines.push("已请求取消".to_string());
            memory.probe_run.lines.truncate(200);
        }
        self.persist(app).await?;
        Ok(self.probe_run_json().await)
    }

    pub(crate) async fn probe_run_json(&self) -> Value {
        let memory = self.memory.read().await;
        serde_json::to_value(&memory.probe_run).unwrap_or_else(|_| {
            json!({
                "running": false,
                "started_at_unix": null,
                "finished_at_unix": null,
                "total": 0,
                "done": 0,
                "lines": []
            })
        })
    }

    /// Execute a bounded best-effort probe walk.  The worker is deliberately
    /// detached from request handling: a slow exit must never hold an admin HTTP
    /// request open, and cancellation is observed between every bucket attempt.
    pub(crate) async fn run_probe(
        self: Arc<Self>,
        app: AppState,
        requested_key_ids: Option<Vec<String>>,
    ) {
        let owner = format!("codex-turn-state:{}", app.tunnel.local_instance_id());
        let lease = match app
            .runtime_state
            .lock_try_acquire(TURN_STATE_PROBE_LOCK_KEY, &owner, TURN_STATE_PROBE_LOCK_TTL)
            .await
        {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                let _ = self
                    .finish_probe(&app, "已有其他网关实例执行探测".to_string())
                    .await;
                return;
            }
            Err(err) => {
                let _ = self
                    .finish_probe(&app, format!("无法取得探测锁: {err}"))
                    .await;
                return;
            }
        };
        let result = self.run_probe_inner(&app, requested_key_ids).await;
        if let Err(err) = result {
            let _ = self.finish_probe(&app, format!("探测失败: {err:?}")).await;
        }
        let _ = app.runtime_state.lock_release(&lease).await;
    }

    async fn run_probe_inner(
        &self,
        app: &AppState,
        requested_key_ids: Option<Vec<String>>,
    ) -> Result<(), GatewayError> {
        let scope = self.scope_raw(app).await?;
        let mut key_ids = requested_key_ids
            .unwrap_or_else(|| scope.key_ids.clone())
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        // Probe accounts in the same order as the runtime policy: already
        // degraded accounts get the first opportunity to recover, followed by
        // suspected accounts, then normal accounts.  The bounded concurrent
        // queue starts with this ordering, so a large scope cannot starve an
        // unhealthy account behind a long list of normal keys.
        {
            let memory = self.memory.read().await;
            key_ids.sort_by_key(|key_id| {
                account_probe_priority(memory.accounts.get(key_id).map(|state| state.verdict))
            });
        }
        key_ids.dedup();
        let models = scope
            .models
            .iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();

        let config = self.config(app).await?;
        let concurrency = config.max_accounts_in_flight.max(1) as usize;
        let jobs = futures_util::stream::iter(key_ids.into_iter().map(|key_id| {
            let scope = &scope;
            let models = &models;
            async move {
                self.probe_account_round(app, scope, key_id.as_str(), models)
                    .await
            }
        }))
        .buffer_unordered(concurrency);
        futures_util::pin_mut!(jobs);
        let mut first_error = None;
        while let Some(result) = jobs.next().await {
            if let Err(err) = result {
                first_error.get_or_insert(err);
            }
        }
        if let Some(err) = first_error {
            return Err(err);
        }
        self.finish_probe(app, "探测完成".to_string()).await
    }

    async fn probe_account_round(
        &self,
        app: &AppState,
        scope: &TurnStateScope,
        key_id: &str,
        models: &[String],
    ) -> Result<(), GatewayError> {
        let mut observation = aether_turn_state::ProbeRoundObservation {
            all_models_degraded: !models.is_empty(),
            recovered_models: Vec::new(),
            degraded_models: Vec::new(),
            completed_at_unix_secs: current_unix_secs(),
        };
        let mut round_observed = false;
        let mut round_complete = !models.is_empty();
        for model in models {
            if !self.probe_is_running().await {
                return Ok(());
            }
            let outcome = self.probe_one(app, scope, key_id, model).await;
            match outcome {
                ProbeOneOutcome::Template {
                    value,
                    issued_at_unix,
                    exit,
                } => {
                    self.store_probe_template(
                        app,
                        key_id,
                        model,
                        value,
                        issued_at_unix,
                        exit.clone(),
                    )
                    .await?;
                    round_observed = true;
                    observation.all_models_degraded = false;
                    observation.recovered_models.push(model.clone());
                    self.advance_probe(
                        app,
                        format!(
                            "{key_id}/{model}: 292{}",
                            exit.as_deref()
                                .map(|value| format!(" via {value}"))
                                .unwrap_or_default()
                        ),
                    )
                    .await?;
                }
                ProbeOneOutcome::Degraded { exit } => {
                    round_observed = true;
                    observation.degraded_models.push(model.clone());
                    self.advance_probe(
                        app,
                        format!(
                            "{key_id}/{model}: 312{}",
                            exit.as_deref()
                                .map(|value| format!(" via {value}"))
                                .unwrap_or_default()
                        ),
                    )
                    .await?;
                }
                ProbeOneOutcome::AccountLimited(status) => {
                    // 401/403/429 are account-level signals, not evidence
                    // that every model is in the 312 state.  Stop the walk
                    // under backoff without feeding this response into the
                    // degraded-round state machine.
                    round_observed = false;
                    round_complete = false;
                    observation.all_models_degraded = false;
                    observation.degraded_models.clear();
                    observation.recovered_models.clear();
                    self.project_probe_auth_failure(app, key_id, status).await?;
                    self.advance_probe(app, format!("{key_id}/{model}: account {status}"))
                        .await?;
                    break;
                }
                ProbeOneOutcome::Skipped(detail) => {
                    // A credential/readability/cooldown skip is not evidence
                    // of a 312 account round.  It is intentionally excluded
                    // from the verdict state machine.
                    observation.all_models_degraded = false;
                    round_complete = false;
                    self.advance_probe(app, format!("{key_id}/{model}: {detail}"))
                        .await?;
                }
            }
        }
        observation.completed_at_unix_secs = current_unix_secs();
        if round_observed && round_complete {
            let config = self.config(app).await?;
            let transition = {
                let mut memory = self.memory.write().await;
                memory
                    .accounts
                    .entry(key_id.to_string())
                    .or_default()
                    .apply_round(observation, config.degrade_threshold)
            };
            self.apply_verdict_health_transition(app, key_id, transition, &config)
                .await?;
        }
        self.persist(app).await
    }

    async fn project_probe_auth_failure(
        &self,
        app: &AppState,
        key_id: &str,
        status: u16,
    ) -> Result<(), GatewayError> {
        if !matches!(status, 401 | 403) {
            return Ok(());
        }
        for _ in 0..4 {
            let Some(key) = app
                .read_provider_catalog_keys_by_ids(&[key_id.to_string()])
                .await?
                .into_iter()
                .find(|key| key.id == key_id)
            else {
                return Ok(());
            };
            let api_format = codex_health_api_format(&key.api_formats);
            let Some(health_by_format) = project_local_failure_health(
                key.health_by_format.as_ref(),
                api_format.as_str(),
                LocalFailoverClassification::RetryStatusCode,
                status,
                current_unix_secs(),
            ) else {
                return Ok(());
            };
            let updated = app
                .compare_and_update_provider_catalog_key_health_state(
                    &ProviderCatalogKeyHealthStateUpdate {
                        key_id: key.id.clone(),
                        expected_encrypted_auth_config: key.encrypted_auth_config.clone(),
                        expected_health_by_format: key.health_by_format.clone(),
                        expected_circuit_breaker_by_format: key.circuit_breaker_by_format.clone(),
                        health_by_format: Some(health_by_format),
                        circuit_breaker_by_format: key.circuit_breaker_by_format.clone(),
                    },
                )
                .await?;
            if updated {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn apply_verdict_health_transition(
        &self,
        app: &AppState,
        key_id: &str,
        transition: VerdictTransition,
        config: &TurnStateConfig,
    ) -> Result<(), GatewayError> {
        let action = match transition {
            VerdictTransition::Degraded => {
                if config.degrade_action == "none" {
                    return Ok(());
                }
                Some(config.degrade_action.as_str().to_string())
            }
            VerdictTransition::Recovered => {
                let memory = self.memory.read().await;
                memory.health_action_applied.get(key_id).cloned()
            }
            VerdictTransition::Unchanged | VerdictTransition::Suspected => None,
        };
        let Some(action) = action else {
            return Ok(());
        };

        let applied = self
            .project_provider_health_action(app, key_id, action.as_str(), transition)
            .await?;
        if !applied {
            return Ok(());
        }

        let mut memory = self.memory.write().await;
        match transition {
            VerdictTransition::Degraded => {
                memory
                    .health_action_applied
                    .insert(key_id.to_string(), action);
            }
            VerdictTransition::Recovered => {
                memory.health_action_applied.remove(key_id);
            }
            VerdictTransition::Unchanged | VerdictTransition::Suspected => {}
        }
        drop(memory);
        self.persist(app).await
    }

    async fn project_provider_health_action(
        &self,
        app: &AppState,
        key_id: &str,
        action: &str,
        transition: VerdictTransition,
    ) -> Result<bool, GatewayError> {
        let action = action.trim();
        if !matches!(action, "downweight" | "disable") {
            return Ok(false);
        }

        for _ in 0..4 {
            let Some(key) = app
                .read_provider_catalog_keys_by_ids(&[key_id.to_string()])
                .await?
                .into_iter()
                .find(|key| key.id == key_id)
            else {
                return Ok(false);
            };
            let api_format = codex_health_api_format(&key.api_formats);
            let (health_by_format, circuit_breaker_by_format) = match (transition, action) {
                (VerdictTransition::Degraded, "downweight") => (
                    project_local_degraded_health(
                        key.health_by_format.as_ref(),
                        api_format.as_str(),
                        current_unix_secs(),
                    ),
                    key.circuit_breaker_by_format.clone(),
                ),
                (VerdictTransition::Degraded, "disable") => (
                    key.health_by_format.clone(),
                    project_local_key_circuit_open(
                        key.circuit_breaker_by_format.as_ref(),
                        api_format.as_str(),
                        "codex_turn_state_degraded",
                        current_unix_secs(),
                        key.max_probe_interval_minutes,
                    ),
                ),
                (VerdictTransition::Recovered, "downweight") => (
                    project_local_success_health(
                        key.health_by_format.as_ref(),
                        api_format.as_str(),
                    ),
                    key.circuit_breaker_by_format.clone(),
                ),
                (VerdictTransition::Recovered, "disable") => (
                    key.health_by_format.clone(),
                    project_local_key_circuit_closed(
                        key.circuit_breaker_by_format.as_ref(),
                        api_format.as_str(),
                    ),
                ),
                _ => return Ok(false),
            };
            let updated = app
                .compare_and_update_provider_catalog_key_health_state(
                    &ProviderCatalogKeyHealthStateUpdate {
                        key_id: key.id.clone(),
                        expected_encrypted_auth_config: key.encrypted_auth_config.clone(),
                        expected_health_by_format: key.health_by_format.clone(),
                        expected_circuit_breaker_by_format: key.circuit_breaker_by_format.clone(),
                        health_by_format,
                        circuit_breaker_by_format,
                    },
                )
                .await?;
            if updated {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn probe_is_running(&self) -> bool {
        self.memory.read().await.probe_run.running
    }

    async fn advance_probe(&self, app: &AppState, line: String) -> Result<(), GatewayError> {
        let mut memory = self.memory.write().await;
        memory.probe_run.done = memory.probe_run.done.saturating_add(1);
        memory.probe_run.lines.push(line);
        memory.probe_run.lines.truncate(200);
        drop(memory);
        self.persist(app).await
    }

    async fn finish_probe(&self, app: &AppState, line: String) -> Result<(), GatewayError> {
        let mut memory = self.memory.write().await;
        memory.probe_run.running = false;
        memory.probe_run.finished_at_unix = Some(current_unix_secs());
        memory.probe_run.lines.push(line);
        memory.probe_run.lines.truncate(200);
        drop(memory);
        self.persist(app).await
    }

    async fn probe_one(
        &self,
        app: &AppState,
        scope: &TurnStateScope,
        key_id: &str,
        model: &str,
    ) -> ProbeOneOutcome {
        let config = self.config(app).await.unwrap_or_default();
        let now = current_unix_secs();
        if self.account_backoff_active(key_id, now).await {
            return ProbeOneOutcome::Skipped("account backoff active".to_string());
        }

        let keys = match app
            .read_provider_catalog_keys_by_ids(&[key_id.to_string()])
            .await
        {
            Ok(keys) => keys,
            Err(_) => return ProbeOneOutcome::Skipped("key read failed".to_string()),
        };
        let Some(key) = keys
            .into_iter()
            .find(|key| key.id == key_id && key.is_active)
        else {
            return ProbeOneOutcome::Skipped("key unavailable".to_string());
        };
        let auth_config = key
            .encrypted_auth_config
            .as_deref()
            .and_then(|_| {
                app.decrypt_provider_catalog_key_auth_config(&key)
                    .ok()
                    .flatten()
            })
            .and_then(|value| serde_json::from_str::<Value>(&value).ok());
        let token = app
            .decrypt_provider_catalog_key_api_key(&key)
            .ok()
            .flatten()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                auth_config
                    .as_ref()
                    .and_then(|value| value.get("access_token"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .or_else(|| {
                auth_config
                    .as_ref()
                    .and_then(|value| value.get("headers"))
                    .and_then(|value| value.get("authorization"))
                    .and_then(Value::as_str)
                    .map(|value| value.trim_start_matches("Bearer ").to_string())
            });
        let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
            return ProbeOneOutcome::Skipped("credential unavailable".to_string());
        };
        let account_id = auth_config
            .as_ref()
            .and_then(|value| value.get("account_id"))
            .and_then(Value::as_str)
            .or_else(|| {
                auth_config
                    .as_ref()
                    .and_then(|value| value.get("headers"))
                    .and_then(|value| value.get("chatgpt-account-id"))
                    .and_then(Value::as_str)
            });

        let mut attempted = false;
        let mut saw_degraded = false;
        let mut last_degraded_exit = None;

        // Static exits are each tried at most once per bucket during their
        // configured cooldown.  A hashed URL is used as the persistence key so
        // proxy credentials never enter runtime storage or Redis-compatible keys.
        let static_start = probe_exit_start_index(key_id, scope.probe_proxies.len());
        for offset in 0..scope.probe_proxies.len() {
            let proxy = &scope.probe_proxies[(static_start + offset) % scope.probe_proxies.len()];
            let cooldown_key = exit_cooldown_key(proxy, key_id, model);
            if self.exit_cooldown_active(cooldown_key.as_str(), now).await {
                continue;
            }
            attempted = true;
            match self
                .send_probe(
                    token.as_str(),
                    account_id,
                    model,
                    Some(proxy.as_str()),
                    &config,
                )
                .await
            {
                ProbeResponse::Template {
                    value,
                    issued_at_unix,
                } => {
                    return ProbeOneOutcome::Template {
                        value,
                        issued_at_unix,
                        exit: Some(mask_proxy(proxy)),
                    };
                }
                ProbeResponse::Degraded => {
                    saw_degraded = true;
                    last_degraded_exit = Some(mask_proxy(proxy));
                    self.set_exit_cooldown(
                        cooldown_key,
                        now.saturating_add(config.exit_cooldown_seconds),
                    )
                    .await;
                }
                ProbeResponse::AccountLimited(status) => {
                    self.set_account_backoff(
                        key_id,
                        now.saturating_add(config.account_backoff_seconds),
                    )
                    .await;
                    return ProbeOneOutcome::AccountLimited(status);
                }
                ProbeResponse::NetworkFailure => {
                    self.set_exit_cooldown(
                        cooldown_key,
                        now.saturating_add(config.exit_cooldown_seconds.min(300)),
                    )
                    .await;
                }
                ProbeResponse::NoTurnState
                | ProbeResponse::UnknownLength(_)
                | ProbeResponse::UpstreamFailure(_) => {}
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        // A rotating gateway represents a shared virtual exit.  Try at most N
        // times and then cool the whole pool for ten minutes; 312 never causes
        // account backoff.
        let rotating_key = rotating_cooldown_key(key_id, model);
        if !scope.probe_proxies_rotating.is_empty()
            && !self
                .rotating_cooldown_active(rotating_key.as_str(), now)
                .await
        {
            let mut rotating_degraded = true;
            for attempt in 0..config.rotating_max_attempts as usize {
                let proxy =
                    &scope.probe_proxies_rotating[attempt % scope.probe_proxies_rotating.len()];
                attempted = true;
                match self
                    .send_probe(
                        token.as_str(),
                        account_id,
                        model,
                        Some(proxy.as_str()),
                        &config,
                    )
                    .await
                {
                    ProbeResponse::Template {
                        value,
                        issued_at_unix,
                    } => {
                        return ProbeOneOutcome::Template {
                            value,
                            issued_at_unix,
                            exit: Some(mask_proxy(proxy)),
                        };
                    }
                    ProbeResponse::Degraded => {
                        saw_degraded = true;
                        last_degraded_exit = Some(mask_proxy(proxy));
                    }
                    ProbeResponse::AccountLimited(status) => {
                        self.set_account_backoff(
                            key_id,
                            now.saturating_add(config.account_backoff_seconds),
                        )
                        .await;
                        return ProbeOneOutcome::AccountLimited(status);
                    }
                    ProbeResponse::NetworkFailure => rotating_degraded = false,
                    ProbeResponse::NoTurnState
                    | ProbeResponse::UnknownLength(_)
                    | ProbeResponse::UpstreamFailure(_) => rotating_degraded = false,
                }
                if attempt + 1 < config.rotating_max_attempts as usize {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
            if rotating_degraded {
                self.set_rotating_cooldown(rotating_key, now.saturating_add(600))
                    .await;
            }
        }

        // With no configured pool, retain the original direct-probe behavior.
        if !attempted && scope.probe_proxies.is_empty() && scope.probe_proxies_rotating.is_empty() {
            match self
                .send_probe(token.as_str(), account_id, model, None, &config)
                .await
            {
                ProbeResponse::Template {
                    value,
                    issued_at_unix,
                } => {
                    return ProbeOneOutcome::Template {
                        value,
                        issued_at_unix,
                        exit: Some("direct".to_string()),
                    };
                }
                ProbeResponse::Degraded => {
                    saw_degraded = true;
                    last_degraded_exit = Some("direct".to_string());
                }
                ProbeResponse::AccountLimited(status) => {
                    self.set_account_backoff(
                        key_id,
                        now.saturating_add(config.account_backoff_seconds),
                    )
                    .await;
                    return ProbeOneOutcome::AccountLimited(status);
                }
                ProbeResponse::NetworkFailure
                | ProbeResponse::NoTurnState
                | ProbeResponse::UnknownLength(_)
                | ProbeResponse::UpstreamFailure(_) => {}
            }
        }

        if saw_degraded {
            ProbeOneOutcome::Degraded {
                exit: last_degraded_exit,
            }
        } else if attempted {
            ProbeOneOutcome::Skipped("未采到正常态".to_string())
        } else {
            ProbeOneOutcome::Skipped("出口冷却中".to_string())
        }
    }

    async fn send_probe(
        &self,
        token: &str,
        account_id: Option<&str>,
        model: &str,
        proxy: Option<&str>,
        config: &TurnStateConfig,
    ) -> ProbeResponse {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy) = proxy.filter(|value| !value.trim().is_empty()) {
            let Ok(proxy) = reqwest::Proxy::all(proxy) else {
                return ProbeResponse::NetworkFailure;
            };
            builder = builder.proxy(proxy);
        }
        let Ok(client) = builder.build() else {
            return ProbeResponse::NetworkFailure;
        };
        let body = json!({
            "model": model,
            "stream": true,
            "store": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "ping"}]
            }],
            "reasoning": {"effort": "low"},
            "tool_choice": "auto",
            "parallel_tool_calls": false
        });
        let request = client
            .post("https://chatgpt.com/backend-api/codex/responses")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .header("originator", "codex-tui")
            .header("user-agent", "aether-codex-turn-state/1")
            .json(&body);
        let request = if let Some(account_id) = account_id.filter(|value| !value.trim().is_empty())
        {
            request.header("chatgpt-account-id", account_id)
        } else {
            request
        };
        let Ok(response) = request.send().await else {
            return ProbeResponse::NetworkFailure;
        };
        let status = response.status().as_u16();
        let header = response
            .headers()
            .iter()
            .find(|(name, _)| name.as_str().eq_ignore_ascii_case("x-codex-turn-state"))
            .and_then(|(_, value)| value.to_str().ok())
            .map(str::trim)
            .map(ToOwned::to_owned);
        // Consume at most the first kilobyte of the read-only SSE response.
        // This keeps a malformed/non-streaming upstream from making the probe
        // buffer an unbounded body while still allowing the server to flush the
        // response before the connection is dropped.
        let mut body_stream = response.bytes_stream();
        let mut prefix_len = 0usize;
        while prefix_len < 1024 {
            let Some(chunk) = body_stream.next().await else {
                break;
            };
            let Ok(chunk) = chunk else {
                break;
            };
            prefix_len = prefix_len.saturating_add(chunk.len());
        }
        match aether_turn_state::classify_probe_response(
            Some(status),
            header.as_ref().map(String::len),
            config.template_length,
            config.replace_length,
        ) {
            aether_turn_state::ProbeAttribution::AccountBackoff => {
                ProbeResponse::AccountLimited(status)
            }
            aether_turn_state::ProbeAttribution::ExitCooldown => ProbeResponse::Degraded,
            aether_turn_state::ProbeAttribution::HarvestedTemplate => {
                let Some(value) = header else {
                    return ProbeResponse::NoTurnState;
                };
                let Ok(issued_at_unix) = issued_at_unix_secs(&value) else {
                    return ProbeResponse::UnknownLength(value.len());
                };
                if !template_usable(issued_at_unix, current_unix_secs(), config.ttl_seconds) {
                    return ProbeResponse::UnknownLength(value.len());
                }
                ProbeResponse::Template {
                    value,
                    issued_at_unix,
                }
            }
            aether_turn_state::ProbeAttribution::NoTurnState => ProbeResponse::NoTurnState,
            aether_turn_state::ProbeAttribution::UnknownLength => {
                ProbeResponse::UnknownLength(header.map(|value| value.len()).unwrap_or_default())
            }
            aether_turn_state::ProbeAttribution::UpstreamFailure => {
                ProbeResponse::UpstreamFailure(status)
            }
            aether_turn_state::ProbeAttribution::NetworkFailure => ProbeResponse::NetworkFailure,
        }
    }

    async fn account_backoff_active(&self, key_id: &str, now: u64) -> bool {
        self.memory
            .read()
            .await
            .account_backoff_until
            .get(key_id)
            .is_some_and(|until| *until > now)
    }

    async fn set_account_backoff(&self, key_id: &str, until: u64) {
        self.memory
            .write()
            .await
            .account_backoff_until
            .insert(key_id.to_string(), until);
    }

    async fn exit_cooldown_active(&self, key: &str, now: u64) -> bool {
        self.memory
            .read()
            .await
            .exit_cooldowns_until
            .get(key)
            .is_some_and(|until| *until > now)
    }

    async fn set_exit_cooldown(&self, key: String, until: u64) {
        self.memory
            .write()
            .await
            .exit_cooldowns_until
            .insert(key, until);
    }

    async fn rotating_cooldown_active(&self, key: &str, now: u64) -> bool {
        self.memory
            .read()
            .await
            .rotating_cooldowns_until
            .get(key)
            .is_some_and(|until| *until > now)
    }

    async fn set_rotating_cooldown(&self, key: String, until: u64) {
        self.memory
            .write()
            .await
            .rotating_cooldowns_until
            .insert(key, until);
    }

    async fn store_probe_template(
        &self,
        app: &AppState,
        key_id: &str,
        model: &str,
        value: String,
        issued_at_unix: u64,
        last_exit: Option<String>,
    ) -> Result<(), GatewayError> {
        let config = self.config(app).await?;
        let key = bucket_key(key_id, model);
        let mut memory = self.memory.write().await;
        let should_store = memory
            .buckets
            .get(&key)
            .map(|existing| issued_at_unix > existing.issued_at_unix)
            .unwrap_or(true);
        if should_store {
            memory.buckets.insert(
                key,
                BucketState {
                    value,
                    issued_at_unix,
                    expires_at_unix: issued_at_unix.saturating_add(config.ttl_seconds),
                    source: "probe".to_string(),
                    last_exit,
                },
            );
            memory.counters.harvest = memory.counters.harvest.saturating_add(1);
        }
        drop(memory);
        self.persist(app).await
    }

    pub(crate) async fn proxy_check(&self, app: &AppState) -> Result<Value, GatewayError> {
        let scope = self.scope_raw(app).await?;
        let mut targets = Vec::new();
        for proxy in scope.probe_proxies {
            targets.push(("static", proxy));
        }
        for proxy in scope.probe_proxies_rotating {
            targets.push(("rotating", proxy));
        }
        if targets.is_empty() {
            targets.push(("static", String::new()));
        }
        let mut results = Vec::with_capacity(targets.len());
        for (pool, raw_proxy) in targets {
            let masked = if raw_proxy.is_empty() {
                "direct".to_string()
            } else {
                mask_proxy(&raw_proxy)
            };
            let mut builder = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none());
            if !raw_proxy.is_empty() {
                let Ok(proxy) = reqwest::Proxy::all(&raw_proxy) else {
                    results.push(json!({
                        "proxy": masked,
                        "pool": pool,
                        "reachable": false,
                        "status_code": null,
                        "detail": "代理 URL 无法解析",
                        "exit_ip": null,
                        "country": null,
                        "cf_colo": null,
                        "warning": null
                    }));
                    continue;
                };
                builder = builder.proxy(proxy);
            }
            let Ok(client) = builder.build() else {
                results.push(json!({
                    "proxy": masked,
                    "pool": pool,
                    "reachable": false,
                    "status_code": null,
                    "detail": "无法建立代理客户端",
                    "exit_ip": null,
                    "country": null,
                    "cf_colo": null,
                    "warning": null
                }));
                continue;
            };
            let first_trace = read_proxy_trace(&client).await;
            let second_trace = if pool == "static" {
                // A static pool is expected to keep one exit.  Two bounded
                // trace reads make a rotation visible to the admin UI without
                // ever exposing proxy credentials.
                read_proxy_trace(&client).await
            } else {
                None
            };
            let exit_ip = first_trace
                .as_ref()
                .and_then(|trace| trace.ip.clone())
                .or_else(|| second_trace.as_ref().and_then(|trace| trace.ip.clone()));
            let country = first_trace
                .as_ref()
                .and_then(|trace| trace.country.clone())
                .or_else(|| {
                    second_trace
                        .as_ref()
                        .and_then(|trace| trace.country.clone())
                });
            let cf_colo = first_trace
                .as_ref()
                .and_then(|trace| trace.cf_colo.clone())
                .or_else(|| {
                    second_trace
                        .as_ref()
                        .and_then(|trace| trace.cf_colo.clone())
                });
            let warning = if pool == "static" {
                static_trace_warning(first_trace.as_ref(), second_trace.as_ref())
            } else {
                None
            };
            let response = client
                .post("https://chatgpt.com/backend-api/codex/responses")
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .json(&json!({
                    "model": "gpt-5.5",
                    "stream": true,
                    "store": false,
                    "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "ping"}]}]
                }))
                .send()
                .await;
            match response {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let detail = match status {
                        401 => "通（上游返回 401）",
                        403 => "已到达上游，但被拒绝（403）",
                        429 => "已到达上游，但被限速（429）",
                        _ => "已收到上游响应",
                    };
                    results.push(json!({
                        "proxy": masked,
                        "pool": pool,
                        "reachable": true,
                        "status_code": status,
                        "detail": detail,
                        "exit_ip": exit_ip,
                        "country": country,
                        "cf_colo": cf_colo,
                        "warning": warning
                    }));
                }
                Err(err) => results.push(json!({
                    "proxy": masked,
                    "pool": pool,
                    "reachable": false,
                    "status_code": null,
                    "detail": format!("无法连接：{}", redact_error(err.to_string().as_str())),
                    "exit_ip": exit_ip,
                    "country": country,
                    "cf_colo": cf_colo,
                    "warning": warning
                })),
            }
        }
        Ok(Value::Array(results))
    }

    pub(crate) async fn status_json(&self, app: &AppState) -> Result<Value, GatewayError> {
        self.ensure_loaded(app).await?;
        let config = self.config(app).await?;
        let now = current_unix_secs();
        let memory = self.memory.read().await;
        let mut buckets = Vec::with_capacity(memory.buckets.len());
        for (key, bucket) in &memory.buckets {
            let Some((key_id, model)) = key.split_once('\0') else {
                continue;
            };
            let ready = bucket.value.len() == config.template_length
                && template_usable(bucket.issued_at_unix, now, config.ttl_seconds);
            buckets.push(json!({
                "key_id": key_id,
                "key_name": key_id,
                "model": model,
                "ready": ready,
                "ttl_remaining_seconds": ready.then(|| bucket.expires_at_unix.saturating_sub(now)),
                "issued_at_unix": bucket.issued_at_unix,
                "expires_at_unix": bucket.expires_at_unix,
                "source": bucket.source,
                "last_exit": bucket.last_exit,
            }));
        }
        let accounts = memory
            .accounts
            .iter()
            .map(|(key_id, state)| {
                json!({
                    "key_id": key_id,
                    "key_name": key_id,
                    "verdict": verdict_name(state.verdict),
                    "consecutive_degraded_rounds": state.consecutive_degraded_rounds,
                    "degraded_models": state.degraded_models,
                    "last_probe_at_unix": state.last_probe_at_unix_secs,
                    "degraded_since_unix": state.degraded_since_unix_secs,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "enabled": config.enabled,
            "dry_run": config.dry_run,
            "buckets": buckets,
            "accounts": accounts,
            "counters": {
                "harvest": memory.counters.harvest,
                "substitute": memory.counters.substitute,
                "inject": memory.counters.inject,
                "pass": memory.counters.pass,
                "skip": memory.counters.skip,
            },
            "counters_since_unix": memory.counters.since_unix,
            "probe_run": memory.probe_run,
        }))
    }

    /// Apply the decision to an already selected execution plan.  This is the
    /// only wire mutation point, so key/model isolation is enforced centrally.
    pub(crate) async fn apply_to_plan(
        &self,
        app: &AppState,
        plan: &mut ExecutionPlan,
        report_context: &mut Option<Value>,
    ) -> Result<(), GatewayError> {
        self.ensure_loaded(app).await?;
        let config = self.config(app).await?;
        if !config.enabled || !is_codex_plan(plan) {
            return Ok(());
        }
        let model = plan
            .model_name
            .as_deref()
            .or_else(|| {
                plan.body
                    .json_body
                    .as_ref()
                    .and_then(|body| body.get("model"))
                    .and_then(Value::as_str)
            })
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_string();
        let key_id = plan.key_id.trim().to_string();
        if key_id.is_empty() || model.is_empty() {
            self.increment_counter(|c| c.skip = c.skip.saturating_add(1))
                .await;
            return Ok(());
        }
        let bucket = {
            let memory = self.memory.read().await;
            memory
                .buckets
                .get(&bucket_key(&key_id, &model))
                .map(|bucket| LiveTemplate {
                    value: bucket.value.clone(),
                    issued_at_unix_secs: bucket.issued_at_unix,
                })
        };
        let current = plan
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-codex-turn-state"))
            .map(|(_, value)| value.as_str());
        let decision = decide_header(TurnStateDecisionInput {
            request_value: current,
            template: bucket.as_ref(),
            now_unix_secs: current_unix_secs(),
            ttl_seconds: config.ttl_seconds,
            template_length: config.template_length,
            replace_length: config.replace_length,
            inject_mode: config.inject_mode,
            dry_run: config.dry_run,
        });
        self.increment_counter(|c| match decision.action {
            DecisionAction::Substitute => c.substitute = c.substitute.saturating_add(1),
            DecisionAction::Inject => c.inject = c.inject.saturating_add(1),
            DecisionAction::Pass => c.pass = c.pass.saturating_add(1),
            DecisionAction::Harvest | DecisionAction::Skip => {}
        })
        .await;
        if decision.applied {
            if let Some(replacement) = decision.replacement.as_deref() {
                let names = plan
                    .headers
                    .keys()
                    .filter(|name| name.eq_ignore_ascii_case("x-codex-turn-state"))
                    .cloned()
                    .collect::<Vec<_>>();
                for name in names {
                    plan.headers.remove(&name);
                }
                plan.headers
                    .insert("x-codex-turn-state".to_string(), replacement.to_string());
            }
        }
        let verdict = self
            .memory
            .read()
            .await
            .accounts
            .get(&key_id)
            .map(|state| verdict_name(state.verdict));
        merge_report_context(
            report_context,
            "turn_state_verdict",
            verdict
                .map(|value| Value::String(value.to_string()))
                .unwrap_or(Value::Null),
        );
        merge_report_context(
            report_context,
            "turn_state_action",
            Value::String(format!("{:?}", decision.action).to_ascii_lowercase()),
        );
        Ok(())
    }

    pub(crate) async fn harvest_response_headers(
        &self,
        app: &AppState,
        report_context: Option<&Value>,
        headers: &BTreeMap<String, String>,
        source: &'static str,
    ) -> Result<TurnStateHarvestResult, GatewayError> {
        self.ensure_loaded(app).await?;
        let config = self.config(app).await?;
        if !config.enabled || !config.harvest_inband || !is_codex_report_context(report_context) {
            return Ok(TurnStateHarvestResult::Disabled);
        }
        let key_id = report_context
            .and_then(|value| value.get("key_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let model = report_context
            .and_then(|value| value.get("mapped_model").or_else(|| value.get("model")))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        if key_id.is_empty() || model.is_empty() {
            return Ok(TurnStateHarvestResult::Ignored);
        }
        let Some(value) = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-codex-turn-state"))
            .map(|(_, value)| value.as_str())
            .map(str::trim)
            .filter(|value| value.len() == config.template_length)
        else {
            return Ok(TurnStateHarvestResult::Ignored);
        };
        let Ok(issued_at_unix) = issued_at_unix_secs(value) else {
            return Ok(TurnStateHarvestResult::Ignored);
        };
        let now = current_unix_secs();
        if !template_usable(issued_at_unix, now, config.ttl_seconds) {
            return Ok(TurnStateHarvestResult::Ignored);
        }
        let key = bucket_key(key_id, model);
        let mut stored = false;
        let mut recovered = false;
        {
            let mut memory = self.memory.write().await;
            let should_store = memory
                .buckets
                .get(&key)
                .map(|existing| issued_at_unix > existing.issued_at_unix)
                .unwrap_or(true);
            if should_store {
                memory.buckets.insert(
                    key,
                    BucketState {
                        value: value.to_string(),
                        issued_at_unix,
                        expires_at_unix: issued_at_unix.saturating_add(config.ttl_seconds),
                        source: source.to_string(),
                        last_exit: report_context
                            .and_then(|context| context.get("last_exit"))
                            .and_then(Value::as_str)
                            .map(mask_proxy),
                    },
                );
                memory.counters.harvest = memory.counters.harvest.saturating_add(1);
                recovered = memory
                    .accounts
                    .entry(key_id.to_string())
                    .or_default()
                    .recover_model(model, now);
                stored = true;
            }
        }
        if stored {
            if recovered {
                let config = self.config(app).await?;
                self.apply_verdict_health_transition(
                    app,
                    key_id,
                    VerdictTransition::Recovered,
                    &config,
                )
                .await?;
            }
            self.persist(app).await?;
            Ok(TurnStateHarvestResult::Stored)
        } else {
            Ok(TurnStateHarvestResult::Older)
        }
    }

    async fn increment_counter(&self, update: impl FnOnce(&mut Counters)) {
        let mut memory = self.memory.write().await;
        if memory.counters.since_unix == 0 {
            memory.counters.since_unix = current_unix_secs();
        }
        update(&mut memory.counters);
    }

    pub(crate) async fn persist_now(&self, app: &AppState) -> Result<(), GatewayError> {
        self.ensure_loaded(app).await?;
        self.persist(app).await
    }
}

fn bucket_key(key_id: &str, model: &str) -> String {
    format!("{}\0{}", key_id.trim(), model.trim())
}

fn codex_health_api_format(api_formats: &Option<Value>) -> String {
    let is_responses = |value: &str| {
        let value = value.trim().to_ascii_lowercase();
        value == "openai:responses" || value == "openai:responses:compact"
    };
    if let Some(value) = api_formats {
        if let Some(values) = value.as_array() {
            if let Some(format) = values
                .iter()
                .filter_map(Value::as_str)
                .find(|format| is_responses(format))
            {
                return format.to_string();
            }
        }
        if let Some(values) = value.as_object() {
            if let Some(format) = values.keys().find(|format| is_responses(format)) {
                return format.clone();
            }
        }
    }
    "openai:responses".to_string()
}

fn exit_cooldown_key(proxy: &str, key_id: &str, model: &str) -> String {
    format!(
        "{}\0{}\0{}",
        hash_probe_exit(proxy),
        key_id.trim(),
        model.trim()
    )
}

fn rotating_cooldown_key(key_id: &str, model: &str) -> String {
    format!("__rotating__\0{}\0{}", key_id.trim(), model.trim())
}

fn hash_probe_exit(proxy: &str) -> String {
    let digest = Sha256::digest(proxy.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn probe_exit_start_index(key_id: &str, pool_len: usize) -> usize {
    if pool_len == 0 {
        return 0;
    }
    let digest = Sha256::digest(key_id.as_bytes());
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    let value = u64::from_be_bytes(prefix);
    (value as usize) % pool_len
}

fn is_codex_plan(plan: &ExecutionPlan) -> bool {
    [plan.provider_name.as_deref(), Some(plan.url.as_str())]
        .into_iter()
        .flatten()
        .any(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("codex") || value.contains("chatgpt.com/backend-api")
        })
}

fn is_codex_report_context(context: Option<&Value>) -> bool {
    let Some(context) = context else {
        return false;
    };
    let named_codex = ["provider_type", "provider_name", "upstream_url"]
        .into_iter()
        .filter_map(|key| context.get(key).and_then(Value::as_str))
        .any(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("codex") || value.contains("chatgpt.com/backend-api")
        });
    if named_codex {
        return true;
    }
    context
        .get("provider_api_format")
        .and_then(Value::as_str)
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "openai:responses" | "openai:responses:compact"
            )
        })
}

fn merge_report_context(context: &mut Option<Value>, key: &str, value: Value) {
    let mut object = match context.take() {
        Some(Value::Object(object)) => object,
        Some(other) => Map::from_iter([("seed".to_string(), other)]),
        None => Map::new(),
    };
    object.insert(key.to_string(), value);
    *context = Some(Value::Object(object));
}

fn verdict_name(verdict: AccountVerdict) -> &'static str {
    match verdict {
        AccountVerdict::Normal => "normal",
        AccountVerdict::Suspected => "suspected",
        AccountVerdict::Degraded => "degraded",
    }
}

fn account_probe_priority(verdict: Option<AccountVerdict>) -> u8 {
    match verdict {
        Some(AccountVerdict::Degraded) => 0,
        Some(AccountVerdict::Suspected) => 1,
        Some(AccountVerdict::Normal) | None => 2,
    }
}

#[derive(Debug, Clone, Default)]
struct ExitTrace {
    ip: Option<String>,
    country: Option<String>,
    cf_colo: Option<String>,
}

async fn read_proxy_trace(client: &reqwest::Client) -> Option<ExitTrace> {
    let response = client
        .get("https://chatgpt.com/cdn-cgi/trace")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.text().await.ok()?;
    let mut trace = ExitTrace::default();
    for line in body.lines().take(64) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(value) = bounded_trace_value(value) else {
            continue;
        };
        match key.trim() {
            "ip" => trace.ip = Some(value),
            "loc" => trace.country = Some(value),
            "colo" => trace.cf_colo = Some(value),
            _ => {}
        }
    }
    if trace.ip.is_none() && trace.country.is_none() && trace.cf_colo.is_none() {
        None
    } else {
        Some(trace)
    }
}

fn bounded_trace_value(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() || value.len() > 64 {
        return None;
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'-' | b'_'))
    {
        return None;
    }
    Some(value.to_string())
}

fn static_trace_warning(first: Option<&ExitTrace>, second: Option<&ExitTrace>) -> Option<String> {
    match (
        first.and_then(|trace| trace.ip.as_deref()),
        second.and_then(|trace| trace.ip.as_deref()),
    ) {
        (Some(first), Some(second)) if first != second => {
            Some("静态出口两次追踪的 IP 不一致，可能并非固定出口".to_string())
        }
        _ => None,
    }
}

fn mask_proxy(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw.trim()) else {
        return "[invalid-proxy]".to_string();
    };
    if url.host_str().is_none() {
        return "[invalid-proxy]".to_string();
    }
    let has_userinfo = !url.username().is_empty() || url.password().is_some();
    if !has_userinfo {
        return url.to_string();
    }
    let _ = url.set_username("***");
    let _ = url.set_password(None);
    url.to_string()
}

fn merge_masked_proxy_values(values: &mut [String], previous: &[String]) {
    for value in values {
        if !value.contains("***") {
            continue;
        }
        let Ok(masked) = Url::parse(value) else {
            continue;
        };
        if let Some(original) = previous.iter().find(|candidate| {
            Url::parse(candidate).ok().is_some_and(|parsed| {
                parsed.scheme() == masked.scheme()
                    && parsed.host_str() == masked.host_str()
                    && parsed.port() == masked.port()
            })
        }) {
            *value = original.clone();
        }
    }
}

fn redact_error(raw: &str) -> String {
    let mut value = raw.replace("authorization", "credential");
    value = value.replace("password", "secret");
    for scheme in ["http://", "https://", "socks5://", "socks5h://"] {
        let mut search_from = 0usize;
        while let Some(relative_start) = value[search_from..].find(scheme) {
            let start = search_from + relative_start;
            let end = value[start..]
                .char_indices()
                .skip(1)
                .find_map(|(offset, character)| {
                    (character.is_whitespace()
                        || matches!(character, ')' | ']' | '}' | '"' | '\'' | ',' | ';'))
                    .then_some(start + offset)
                })
                .unwrap_or(value.len());
            let candidate = value[start..end].to_string();
            let Ok(parsed) = Url::parse(&candidate) else {
                search_from = end;
                continue;
            };
            if parsed.username().is_empty() && parsed.password().is_none() {
                search_from = end;
                continue;
            }
            let masked = mask_proxy(&candidate);
            value.replace_range(start..end, &masked);
            search_from = start + masked.len();
        }
    }
    if value.len() > 240 {
        value.truncate(240);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::redact_error;

    #[test]
    fn proxy_credentials_are_redacted_from_error_details() {
        let message = redact_error(
            "request failed for (https://alice:secret@example.com:8080/path), retrying",
        );
        assert!(!message.contains("alice"));
        assert!(!message.contains("secret"));
        assert!(message.contains("***@example.com:8080/path"));
    }
}
