#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeAttribution {
    /// 429/401/403 must stop the account walk and use account backoff.
    AccountBackoff,
    /// A normal response carrying the known degraded length only cools the
    /// exit; it must never increment account backoff.
    ExitCooldown,
    HarvestedTemplate,
    NoTurnState,
    UnknownLength,
    UpstreamFailure,
    NetworkFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeCooldown {
    pub account_backoff_seconds: u64,
    pub exit_cooldown_seconds: u64,
    pub network_cooldown_seconds: u64,
    pub rotating_cooldown_seconds: u64,
}

impl Default for ProbeCooldown {
    fn default() -> Self {
        Self {
            account_backoff_seconds: 600,
            exit_cooldown_seconds: 3300,
            network_cooldown_seconds: 300,
            rotating_cooldown_seconds: 600,
        }
    }
}

/// Classify a probe response.  The status-code branch is intentionally checked
/// before the body length so 429/401/403 can never be mistaken for an exit-level
/// 312 signal.
pub fn classify_probe_response(
    status_code: Option<u16>,
    turn_state_len: Option<usize>,
    template_length: usize,
    replace_length: usize,
) -> ProbeAttribution {
    match status_code {
        Some(401 | 403 | 429) => ProbeAttribution::AccountBackoff,
        Some(200..=299) => match turn_state_len {
            Some(length) if length == template_length => ProbeAttribution::HarvestedTemplate,
            Some(length) if length == replace_length => ProbeAttribution::ExitCooldown,
            Some(_) => ProbeAttribution::UnknownLength,
            None => ProbeAttribution::NoTurnState,
        },
        None => ProbeAttribution::NetworkFailure,
        Some(_) => ProbeAttribution::UpstreamFailure,
    }
}

pub fn cooldown_seconds(attribution: ProbeAttribution, cooldown: ProbeCooldown) -> Option<u64> {
    match attribution {
        ProbeAttribution::AccountBackoff => Some(cooldown.account_backoff_seconds),
        ProbeAttribution::ExitCooldown => Some(cooldown.exit_cooldown_seconds),
        ProbeAttribution::NetworkFailure => Some(cooldown.network_cooldown_seconds),
        ProbeAttribution::HarvestedTemplate
        | ProbeAttribution::NoTurnState
        | ProbeAttribution::UnknownLength
        | ProbeAttribution::UpstreamFailure => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_errors_are_not_exit_cooldowns() {
        for status in [401, 403, 429] {
            let attribution = classify_probe_response(Some(status), Some(312), 292, 312);
            assert_eq!(attribution, ProbeAttribution::AccountBackoff);
            assert_eq!(
                cooldown_seconds(attribution, ProbeCooldown::default()),
                Some(600)
            );
        }
    }

    #[test]
    fn exit_level_312_is_only_for_successful_responses() {
        let attribution = classify_probe_response(Some(200), Some(312), 292, 312);
        assert_eq!(attribution, ProbeAttribution::ExitCooldown);
        assert_eq!(
            cooldown_seconds(attribution, ProbeCooldown::default()),
            Some(3300)
        );
    }

    #[test]
    fn unknown_and_missing_lengths_are_not_stored() {
        assert_eq!(
            classify_probe_response(Some(200), Some(301), 292, 312),
            ProbeAttribution::UnknownLength
        );
        assert_eq!(
            classify_probe_response(Some(200), None, 292, 312),
            ProbeAttribution::NoTurnState
        );
    }
}
