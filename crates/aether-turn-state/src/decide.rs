use serde::{Deserialize, Serialize};

use crate::fernet::template_usable;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum InjectMode {
    #[default]
    ReplaceOnly,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionAction {
    Harvest,
    Substitute,
    Inject,
    Pass,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTemplate {
    pub value: String,
    pub issued_at_unix_secs: u64,
}

impl LiveTemplate {
    pub fn is_usable(&self, now_unix_secs: u64, ttl_seconds: u64, template_length: usize) -> bool {
        self.value.len() == template_length
            && template_usable(self.issued_at_unix_secs, now_unix_secs, ttl_seconds)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TurnStateDecisionInput<'a> {
    pub request_value: Option<&'a str>,
    pub template: Option<&'a LiveTemplate>,
    pub now_unix_secs: u64,
    pub ttl_seconds: u64,
    pub template_length: usize,
    pub replace_length: usize,
    pub inject_mode: InjectMode,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderDecision {
    pub action: DecisionAction,
    /// `None` means leave the request header untouched.  A replacement is
    /// present even during dry-run so callers can count/log the same decision;
    /// the caller must honor `applied` before mutating the wire request.
    pub replacement: Option<String>,
    pub applied: bool,
    pub reason: &'static str,
}

impl HeaderDecision {
    fn pass(reason: &'static str) -> Self {
        Self {
            action: DecisionAction::Pass,
            replacement: None,
            applied: false,
            reason,
        }
    }
}

/// Decide whether the one Codex header should be kept, substituted, or added.
///
/// The bucket is checked against the token's embedded timestamp again here,
/// rather than trusting a caller's `ready` bit.  This keeps every injection path
/// fail-closed at the last possible point.
pub fn decide_header(input: TurnStateDecisionInput<'_>) -> HeaderDecision {
    let current = input.request_value.unwrap_or_default();
    let Some(template) = input.template.filter(|template| {
        template.is_usable(
            input.now_unix_secs,
            input.ttl_seconds,
            input.template_length,
        )
    }) else {
        return HeaderDecision::pass(if current.len() == input.replace_length {
            "no live template"
        } else {
            "no action"
        });
    };

    if current == template.value {
        return HeaderDecision::pass("already current template");
    }

    let (action, reason) = match input.inject_mode {
        InjectMode::Always => (
            DecisionAction::Inject,
            if current.is_empty() {
                "added"
            } else if current.len() == input.replace_length {
                "replaced degraded state"
            } else {
                "replaced non-template state"
            },
        ),
        InjectMode::ReplaceOnly if !current.is_empty() && current.len() == input.replace_length => {
            (DecisionAction::Substitute, "replaced degraded state")
        }
        InjectMode::ReplaceOnly => return HeaderDecision::pass("replace-only left request alone"),
    };

    HeaderDecision {
        action,
        replacement: Some(template.value.clone()),
        applied: !input.dry_run,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> LiveTemplate {
        LiveTemplate {
            value: "t".repeat(292),
            issued_at_unix_secs: 100,
        }
    }

    fn input<'a>(request_value: Option<&'a str>, mode: InjectMode) -> TurnStateDecisionInput<'a> {
        TurnStateDecisionInput {
            request_value,
            template: None,
            now_unix_secs: 101,
            ttl_seconds: 3600,
            template_length: 292,
            replace_length: 312,
            inject_mode: mode,
            dry_run: false,
        }
    }

    #[test]
    fn replace_only_only_substitutes_312() {
        let template = template();
        let degraded_value = "d".repeat(312);
        let mut degraded = input(Some(&degraded_value), InjectMode::ReplaceOnly);
        degraded.template = Some(&template);
        let decision = decide_header(degraded);
        assert_eq!(decision.action, DecisionAction::Substitute);
        assert!(decision.applied);

        let mut no_header = input(None, InjectMode::ReplaceOnly);
        no_header.template = Some(&template);
        assert_eq!(decide_header(no_header).action, DecisionAction::Pass);
    }

    #[test]
    fn always_adds_or_replaces_and_dry_run_does_not_apply() {
        let template = template();
        let mut input = input(None, InjectMode::Always);
        input.template = Some(&template);
        let decision = decide_header(input);
        assert_eq!(decision.action, DecisionAction::Inject);
        assert!(decision.applied);

        input.dry_run = true;
        let decision = decide_header(input);
        assert!(!decision.applied);
        assert_eq!(decision.replacement.as_deref().map(str::len), Some(292));
    }

    #[test]
    fn future_or_expired_template_is_never_used() {
        let mut future = template();
        future.issued_at_unix_secs = 200;
        let degraded_value = "d".repeat(312);
        let mut input = input(Some(&degraded_value), InjectMode::Always);
        input.template = Some(&future);
        assert_eq!(decide_header(input).action, DecisionAction::Pass);

        let mut expired = template();
        expired.issued_at_unix_secs = 0;
        input.template = Some(&expired);
        input.now_unix_secs = 3600;
        assert_eq!(decide_header(input).action, DecisionAction::Pass);
    }
}
