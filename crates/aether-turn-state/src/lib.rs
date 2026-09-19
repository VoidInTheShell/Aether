//! Transport- and storage-independent policy for Codex `X-Codex-Turn-State`.
//!
//! The gateway owns IO, persistence, and scheduling.  This crate deliberately
//! contains only byte-level parsing and deterministic state transitions so the
//! safety rules can be tested without a database or an HTTP client.

pub mod decide;
pub mod fernet;
pub mod schedule;
pub mod verdict;

pub const TURN_STATE_HEADER: &str = "x-codex-turn-state";
pub const DEFAULT_TEMPLATE_LENGTH: usize = 292;
pub const DEFAULT_REPLACE_LENGTH: usize = 312;
pub const DEFAULT_TTL_SECONDS: u64 = 3_600;

pub use decide::{
    decide_header, DecisionAction, HeaderDecision, InjectMode, LiveTemplate, TurnStateDecisionInput,
};
pub use fernet::{issued_at_unix_secs, template_usable, FernetTimestampError};
pub use schedule::{classify_probe_response, ProbeAttribution, ProbeCooldown};
pub use verdict::{AccountVerdict, AccountVerdictState, ProbeRoundObservation, VerdictTransition};
