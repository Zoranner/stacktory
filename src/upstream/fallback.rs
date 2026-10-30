//! Group source fallback policy.
//!
//! A group moves to the next member only when the failure belongs to the
//! source: network/transport failures, source-level access restrictions,
//! missing objects, timeouts, rate limits, and server errors. Request
//! semantics failures stay terminal. Adapters add their own protocol-response
//! failures (malformed payload, integrity mismatch) to the same group path.
//! Direct proxy entries never cross source boundaries.

use super::{fetch::FetchError, pool::SourcePoolError};

pub(crate) fn status_is_group_fallback(status: u16) -> bool {
    matches!(status, 401 | 403 | 404 | 408 | 429 | 500..=599)
}

pub(crate) fn source_error_is_group_fallback(error: &SourcePoolError) -> bool {
    match error {
        SourcePoolError::AllSourcesFailed { .. } => true,
        SourcePoolError::TerminalSourceError { status, .. } => status_is_group_fallback(*status),
        SourcePoolError::FetcherBuild(_)
        | SourcePoolError::UnknownEntry(_)
        | SourcePoolError::InvalidRequestKey(_)
        | SourcePoolError::InvalidConfiguredUrl(_) => false,
    }
}

pub(crate) fn source_error_is_not_found(error: &SourcePoolError) -> bool {
    matches!(
        error,
        SourcePoolError::TerminalSourceError { status: 404, .. }
            | SourcePoolError::AllSourcesFailed {
                status: Some(404),
                ..
            }
    )
}

pub(crate) fn source_failure_category(error: &SourcePoolError) -> &'static str {
    if source_error_is_not_found(error) {
        "not_found"
    } else {
        "source_failed"
    }
}

pub(crate) fn fetch_error_is_group_fallback(error: &FetchError) -> bool {
    error.is_retryable() || error.status_code().is_some_and(status_is_group_fallback)
}

#[cfg(test)]
mod tests {
    use super::status_is_group_fallback;
    use axum::http::StatusCode;

    #[test]
    fn group_fallback_covers_missing_auth_timeout_rate_limit_and_server_errors() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
        ] {
            assert!(status_is_group_fallback(status.as_u16()), "{status}");
        }
    }

    #[test]
    fn group_fallback_rejects_client_protocol_errors() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::NOT_ACCEPTABLE,
            StatusCode::CONFLICT,
            StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert!(!status_is_group_fallback(status.as_u16()), "{status}");
        }
    }
}
