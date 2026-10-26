//! Group 入口的成员顺序、失败收集和回退结果。

use std::collections::VecDeque;

use super::{pool::SourcePool, pool::SourcePoolError};
use crate::SourceConfig;

#[derive(Clone)]
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

#[cfg(test)]
mod tests {
    use super::GroupRun;
    use url::Url;

    use crate::{SourceConfig, SourceGraph, SourcePool, SourceType};

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
            crate::upstream::SourcePoolError::AllSourcesFailed {
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
            crate::upstream::SourcePoolError::AllSourcesFailed { status: None, .. }
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
