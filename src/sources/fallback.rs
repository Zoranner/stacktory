//! Group source fallback policy.
//!
//! A group moves to the next member only when the failure belongs to the
//! source: network/transport failures, source-level access restrictions,
//! missing objects, timeouts, rate limits, and server errors. Request
//! semantics failures stay terminal. Adapters add their own protocol-response
//! failures (malformed payload, integrity mismatch) to the same group path.
//! Direct proxy entries never cross source boundaries.

use std::collections::VecDeque;

use crate::SourceConfig;

use super::{fetcher::FetchError, pool::SourcePoolError};

use super::pool::SourcePool;

pub(crate) struct GroupRun {
    entry_name: String,
    is_group: bool,
    members: VecDeque<SourceConfig>,
    saw_failure: bool,
    all_not_found: bool,
    failures: Vec<String>,
}

impl GroupRun {
    pub(crate) fn new(
        pool: &SourcePool,
        entry_name: &str,
        preferred_source: Option<&str>,
    ) -> Result<Self, SourcePoolError> {
        let mut members: VecDeque<SourceConfig> = pool
            .proxy_members(entry_name)
            .ok_or_else(|| SourcePoolError::UnknownEntry(entry_name.to_owned()))?
            .into();
        if let Some(preferred_source) = preferred_source
            && let Some(index) = members
                .iter()
                .position(|member| member.name == preferred_source)
        {
            let preferred = members
                .remove(index)
                .expect("preferred source exists in the member list");
            members.insert(0, preferred);
        }
        Ok(Self {
            entry_name: entry_name.to_owned(),
            is_group: pool.is_group(entry_name),
            members,
            saw_failure: false,
            all_not_found: true,
            failures: Vec::new(),
        })
    }

    pub(crate) fn is_group(&self) -> bool {
        self.is_group
    }

    pub(crate) fn next_member(&mut self) -> Option<SourceConfig> {
        self.members.pop_front()
    }

    pub(crate) fn record_failure(&mut self, source_name: &str, error: &str, not_found: bool) {
        self.saw_failure = true;
        if !not_found {
            self.all_not_found = false;
        }
        self.failures.push(format!("{source_name}: {error}"));
    }

    pub(crate) fn has_failures(&self) -> bool {
        self.saw_failure
    }

    pub(crate) fn all_not_found(&self) -> bool {
        self.all_not_found
    }

    pub(crate) fn failure_summary(&self) -> String {
        self.failures.join("; ")
    }

    pub(crate) fn all_sources_failed(&self, request_key: impl Into<String>) -> SourcePoolError {
        SourcePoolError::AllSourcesFailed {
            entry_name: self.entry_name.clone(),
            request_key: request_key.into(),
            failures: self.failure_summary(),
            status: (self.is_group && self.saw_failure && self.all_not_found).then_some(404),
        }
    }
}

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

pub(crate) fn fetch_error_is_group_fallback(error: &FetchError) -> bool {
    error.is_retryable() || error.status_code().is_some_and(status_is_group_fallback)
}

#[cfg(test)]
mod tests {
    use super::{GroupRun, status_is_group_fallback};
    use axum::http::StatusCode;
    use url::Url;

    use crate::{SourceConfig, SourceGraph, SourcePool, SourceType};

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

    #[test]
    fn group_run_preserves_order_and_prefers_a_bound_source() {
        let pool = pool();
        let mut run = GroupRun::new(&pool, "group", Some("second")).expect("group candidate run");
        assert!(run.is_group());
        assert_eq!(run.next_member().expect("first candidate").name, "second");
        assert_eq!(run.next_member().expect("second candidate").name, "first");
        assert!(run.next_member().is_none());
    }

    #[test]
    fn group_run_only_reports_not_found_when_every_failure_is_404() {
        let pool = pool();
        let mut run = GroupRun::new(&pool, "group", None).expect("group candidate run");
        run.record_failure("first", "missing", true);
        run.record_failure("second", "missing", true);
        assert!(run.all_not_found());
        assert!(matches!(
            run.all_sources_failed("object"),
            crate::sources::SourcePoolError::AllSourcesFailed {
                status: Some(404),
                ..
            }
        ));

        let mut run = GroupRun::new(&pool, "group", None).expect("group candidate run");
        run.record_failure("first", "forbidden", false);
        run.record_failure("second", "missing", true);
        assert!(!run.all_not_found());
        assert!(matches!(
            run.all_sources_failed("object"),
            crate::sources::SourcePoolError::AllSourcesFailed { status: None, .. }
        ));
    }

    fn pool() -> SourcePool {
        SourcePool::new(
            "test",
            SourceGraph::new(vec![
                proxy("first", "https://first.example"),
                proxy("second", "https://second.example"),
                SourceConfig {
                    name: "group".to_owned(),
                    source_type: SourceType::Group,
                    base_url: None,
                    require_https: false,
                    members: vec!["first".to_owned(), "second".to_owned()],
                },
            ])
            .expect("source graph"),
        )
        .expect("source pool")
    }

    fn proxy(name: &str, base_url: &str) -> SourceConfig {
        SourceConfig {
            name: name.to_owned(),
            source_type: SourceType::Proxy,
            base_url: Some(Url::parse(base_url).expect("base URL")),
            require_https: false,
            members: Vec::new(),
        }
    }
}
