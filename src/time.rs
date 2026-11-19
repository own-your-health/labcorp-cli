//! The clock, in the forms labcorp writes: Unix seconds and RFC 3339 UTC.

use pk_cli_core::dates;

/// Now, Unix seconds.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Now, RFC 3339 UTC.
pub fn now_rfc3339() -> String {
    rfc3339(now())
}

/// `secs` (Unix seconds), RFC 3339 UTC.
pub fn rfc3339(secs: u64) -> String {
    dates::fmt_rfc3339(i64::try_from(secs).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_print_as_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_767_225_600), "2026-01-01T00:00:00Z");
    }
}
