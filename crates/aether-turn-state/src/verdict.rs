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
}

impl Default for AccountVerdictState {
    fn default() -> Self {
        Self {
            verdict: AccountVerdict::Normal,
            consecutive_degraded_rounds: 0,
            degraded_models: Vec::new(),
            last_probe_at_unix_secs: None,
            degraded_since_unix_secs: None,
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
}
