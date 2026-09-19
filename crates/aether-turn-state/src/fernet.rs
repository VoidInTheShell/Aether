use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

/// Errors returned while reading the unencrypted Fernet envelope metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FernetTimestampError {
    #[error("turn-state value is empty")]
    Empty,
    #[error("turn-state value is not valid base64url")]
    InvalidBase64,
    #[error("turn-state value is shorter than a Fernet header")]
    TooShort,
    #[error("turn-state value has an unsupported Fernet version")]
    UnsupportedVersion,
}

/// Extract the Unix issuance timestamp from a Codex Fernet token.
///
/// Codex sends a single URL-safe base64 Fernet envelope.  Padding is optional
/// in practice, so both padded and raw forms are accepted.  Cryptographic
/// signature verification intentionally does not happen here: the gateway does
/// not possess the upstream Fernet key, and the timestamp is only a TTL anchor.
pub fn issued_at_unix_secs(value: &str) -> Result<u64, FernetTimestampError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(FernetTimestampError::Empty);
    }
    let raw_value = value.trim_end_matches('=');
    let raw = URL_SAFE_NO_PAD
        .decode(raw_value)
        .map_err(|_| FernetTimestampError::InvalidBase64)?;
    if raw.len() < 9 {
        return Err(FernetTimestampError::TooShort);
    }
    if raw[0] != 0x80 {
        return Err(FernetTimestampError::UnsupportedVersion);
    }
    let mut timestamp = [0_u8; 8];
    timestamp.copy_from_slice(&raw[1..9]);
    Ok(u64::from_be_bytes(timestamp))
}

/// A token is usable only before its exact expiry boundary and never when its
/// embedded timestamp is in the future.
pub fn template_usable(issued_at_unix_secs: u64, now_unix_secs: u64, ttl_seconds: u64) -> bool {
    issued_at_unix_secs <= now_unix_secs
        && now_unix_secs < issued_at_unix_secs.saturating_add(ttl_seconds)
}

pub fn expires_at_unix_secs(issued_at_unix_secs: u64, ttl_seconds: u64) -> u64 {
    issued_at_unix_secs.saturating_add(ttl_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE;

    fn token(timestamp: u64, version: u8) -> String {
        let mut bytes = vec![0_u8; 32];
        bytes[0] = version;
        bytes[1..9].copy_from_slice(&timestamp.to_be_bytes());
        URL_SAFE.encode(bytes)
    }

    #[test]
    fn parses_padded_and_raw_tokens() {
        let padded = token(1_700_000_000, 0x80);
        let raw = padded.trim_end_matches('=').to_string();
        assert_eq!(issued_at_unix_secs(&padded), Ok(1_700_000_000));
        assert_eq!(issued_at_unix_secs(&raw), Ok(1_700_000_000));
    }

    #[test]
    fn rejects_invalid_version_and_short_values() {
        assert_eq!(
            issued_at_unix_secs(&token(1, 0)),
            Err(FernetTimestampError::UnsupportedVersion)
        );
        let short = URL_SAFE.encode([0x80_u8, 0, 1]);
        assert_eq!(
            issued_at_unix_secs(&short),
            Err(FernetTimestampError::TooShort)
        );
        assert_eq!(
            issued_at_unix_secs("not base64!!!"),
            Err(FernetTimestampError::InvalidBase64)
        );
    }

    #[test]
    fn usable_boundaries_are_strict() {
        assert!(template_usable(100, 100, 1));
        assert!(!template_usable(100, 101, 1));
        assert!(!template_usable(101, 100, 1));
    }
}
