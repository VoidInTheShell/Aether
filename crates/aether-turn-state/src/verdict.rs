use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountVerdict {
    Normal,
    Suspected,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountVerdictState {
    pub verdict: AccountVerdict,
    pub consecutive_degraded_rounds: u32,
    pub degraded_models: Vec<String>,
    pub last_probe_at_unix_secs: Option<u64>,
    pub degraded_since_unix_secs: Option<u64>,
    /// Models the upstream rejects outright (probe 400, e.g. a bare model
    /// name that is not usable with a ChatGPT account).  They can neither
    /// carry a template nor be degraded, so they are excluded from the
    /// degradation-flag denominator.  A later 292/312 observation clears
    /// the mark.
    #[serde(default)]
    pub unsupported_models: Vec<String>,
}

impl Default for AccountVerdictState {
    fn default() -> Self {
        Self {
            verdict: AccountVerdict::Normal,
            consecutive_degraded_rounds: 0,
            degraded_models: Vec::new(),
            last_probe_at_unix_secs: None,
            degraded_since_unix_secs: None,
            unsupported_models: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRoundObservation {
    pub all_models_degraded: bool,
    pub recovered_models: Vec<String>,
    pub degraded_models: Vec<String>,
    pub completed_at_unix_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictTransition {
    Unchanged,
    Recovered,
    Suspected,
    Degraded,
}

impl AccountVerdictState {
    /// Apply one completed account walk.  A walk with no emitted request (for
    /// example, unreadable credentials) should not call this method.
    pub fn apply_round(
        &mut self,
        observation: ProbeRoundObservation,
        degrade_threshold: u32,
    ) -> VerdictTransition {
        let previous = self.verdict;
        self.last_probe_at_unix_secs = Some(observation.completed_at_unix_secs);

        for model in observation.recovered_models {
            self.degraded_models.retain(|item| item != &model);
        }
        for model in observation.degraded_models {
            if !self.degraded_models.iter().any(|item| item == &model) {
                self.degraded_models.push(model);
            }
        }
        self.degraded_models.sort();

        if observation.all_models_degraded {
            self.consecutive_degraded_rounds = self.consecutive_degraded_rounds.saturating_add(1);
        } else {
            self.consecutive_degraded_rounds = 0;
        }

        self.verdict = if self.consecutive_degraded_rounds == 0 {
            AccountVerdict::Normal
        } else if self.consecutive_degraded_rounds >= degrade_threshold.max(1) {
            AccountVerdict::Degraded
        } else {
            AccountVerdict::Suspected
        };

        match (previous, self.verdict) {
            (AccountVerdict::Degraded, AccountVerdict::Normal) => {
                self.degraded_since_unix_secs = None;
                VerdictTransition::Recovered
            }
            (_, AccountVerdict::Degraded) if previous != AccountVerdict::Degraded => {
                self.degraded_since_unix_secs = Some(observation.completed_at_unix_secs);
                VerdictTransition::Degraded
            }
            (_, AccountVerdict::Suspected) if previous != AccountVerdict::Suspected => {
                VerdictTransition::Suspected
            }
            _ => {
                if self.verdict == AccountVerdict::Normal {
                    self.degraded_since_unix_secs = None;
                }
                VerdictTransition::Unchanged
            }
        }
    }

    /// A passive or probe 292 is a recovery signal independent of the current
    /// configured health action.
    pub fn recover_model(&mut self, model: &str, observed_at_unix_secs: u64) -> bool {
        let existed = self.degraded_models.iter().any(|item| item == model);
        self.degraded_models.retain(|item| item != model);
        if existed || self.verdict != AccountVerdict::Normal {
            self.verdict = AccountVerdict::Normal;
            self.consecutive_degraded_rounds = 0;
            self.degraded_since_unix_secs = None;
            self.last_probe_at_unix_secs = Some(observed_at_unix_secs);
            true
        } else {
            false
        }
    }

    /// Record that the upstream accepts this (account, model) pair — any
    /// 292/312 observation proves the earlier 400 was transient or stale.
    pub fn mark_model_supported(&mut self, model: &str) -> bool {
        let before = self.unsupported_models.len();
        self.unsupported_models.retain(|item| item != model);
        self.unsupported_models.sort();
        before != self.unsupported_models.len()
    }

    /// Record that the upstream rejects this model outright (probe 400).
    pub fn mark_model_unsupported(&mut self, model: &str) -> bool {
        if self.unsupported_models.iter().any(|item| item == model) {
            return false;
        }
        self.unsupported_models.push(model.to_string());
        self.unsupported_models.sort();
        true
    }
}

/// Account-level degradation flag for pool display, derived from the
/// per-model matrix:
///
/// * `None`   — no model is degraded, or every tracked model already has a
///   usable template (the account is fully protected);
/// * `"degraded"` — some model is degraded and the account holds no usable
///   template at all;
/// * `"partial"` — some model is degraded without protection, while at
///   least one other model still has a usable template.
///
/// `unsupported` models (probe 400) are excluded from the denominator: they
/// can never hold a template nor be degraded.
pub fn degradation_flag(
    tracked_models: &[String],
    degraded_models: &[String],
    ready_models: &[String],
    unsupported_models: &[String],
) -> Option<&'static str> {
    let tracked: Vec<&str> = tracked_models
        .iter()
        .map(String::as_str)
        .filter(|model| !unsupported_models.iter().any(|item| item == model))
        .collect();
    if tracked.is_empty() {
        return None;
    }
    let is_degraded = |model: &str| degraded_models.iter().any(|item| item == model);
    if !tracked.iter().any(|model| is_degraded(model)) {
        return None;
    }
    let has_template = |model: &str| ready_models.iter().any(|item| item == model);
    if tracked.iter().all(|model| has_template(model)) {
        return None;
    }
    if tracked.iter().any(|model| has_template(model)) {
        Some("partial")
    } else {
        Some("degraded")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn degraded_round(at: u64) -> ProbeRoundObservation {
        ProbeRoundObservation {
            all_models_degraded: true,
            recovered_models: Vec::new(),
            degraded_models: vec!["gpt-5.5".to_string()],
            completed_at_unix_secs: at,
        }
    }

    #[test]
    fn rounds_progress_normal_suspected_degraded() {
        let mut state = AccountVerdictState::default();
        assert_eq!(
            state.apply_round(degraded_round(1), 3),
            VerdictTransition::Suspected
        );
        assert_eq!(state.verdict, AccountVerdict::Suspected);
        assert_eq!(
            state.apply_round(degraded_round(2), 3),
            VerdictTransition::Unchanged
        );
        assert_eq!(
            state.apply_round(degraded_round(3), 3),
            VerdictTransition::Degraded
        );
        assert_eq!(state.degraded_since_unix_secs, Some(3));
    }

    #[test]
    fn recovery_clears_degraded_since_without_threshold_change() {
        let mut state = AccountVerdictState::default();
        for at in 1..=3 {
            state.apply_round(degraded_round(at), 3);
        }
        assert!(state.recover_model("gpt-5.5", 10));
        assert_eq!(state.verdict, AccountVerdict::Normal);
        assert_eq!(state.consecutive_degraded_rounds, 0);
        assert!(state.degraded_since_unix_secs.is_none());
    }

    #[test]
    fn unsupported_models_default_deserializes_for_legacy_snapshots() {
        let legacy = serde_json::json!({
            "verdict": "normal",
            "consecutive_degraded_rounds": 0,
            "degraded_models": [],
            "last_probe_at_unix_secs": null,
            "degraded_since_unix_secs": null
        });
        let state: AccountVerdictState = serde_json::from_value(legacy).expect("legacy snapshot");
        assert!(state.unsupported_models.is_empty());
    }

    #[test]
    fn unsupported_marks_round_trip_and_clear() {
        let mut state = AccountVerdictState::default();
        assert!(state.mark_model_unsupported("gpt-5.6"));
        assert!(!state.mark_model_unsupported("gpt-5.6"));
        let encoded = serde_json::to_string(&state).expect("serialize");
        assert!(encoded.contains("unsupported_models"));
        assert!(state.mark_model_supported("gpt-5.6"));
        assert!(state.unsupported_models.is_empty());
    }

    fn model_list(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn degradation_flag_follows_the_pool_display_rules() {
        // No degradation observed: flag hidden.
        assert_eq!(
            degradation_flag(&model_list(&["gpt-5.5", "gpt-5.6-luna"]), &[], &[], &[]),
            None
        );
        // Degraded model fully protected by its own template: flag hidden.
        assert_eq!(
            degradation_flag(
                &model_list(&["gpt-5.5", "gpt-5.6-luna"]),
                &model_list(&["gpt-5.6-luna"]),
                &model_list(&["gpt-5.5", "gpt-5.6-luna"]),
                &[]
            ),
            None
        );
        // Degraded model unprotected, sibling protected: partial.
        assert_eq!(
            degradation_flag(
                &model_list(&["gpt-5.5", "gpt-5.6-luna"]),
                &model_list(&["gpt-5.6-luna"]),
                &model_list(&["gpt-5.5"]),
                &[]
            ),
            Some("partial")
        );
        // Degraded model, account holds no template at all: degraded.
        assert_eq!(
            degradation_flag(
                &model_list(&["gpt-5.5", "gpt-5.6-luna"]),
                &model_list(&["gpt-5.6-luna"]),
                &[],
                &[]
            ),
            Some("degraded")
        );
        // Unsupported models (probe 400) leave the denominator so a dead
        // catalog entry can never pin the flag at partial forever.
        assert_eq!(
            degradation_flag(
                &model_list(&["gpt-5.6", "gpt-5.6-luna"]),
                &model_list(&["gpt-5.6-luna"]),
                &model_list(&["gpt-5.6-luna"]),
                &model_list(&["gpt-5.6"])
            ),
            None
        );
        // No tracked models at all: nothing to report.
        assert_eq!(
            degradation_flag(&[], &model_list(&["gpt-5.5"]), &[], &[]),
            None
        );
    }
}
