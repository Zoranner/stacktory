use super::*;

impl NpmService {
    pub(super) async fn search(
        &self,
        entry_name: &str,
        query: Option<&str>,
        headers: &HeaderMap,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, NpmError> {
        let query = validate_search_query(query)?;
        let mut run = GroupRun::new(pool, entry_name, None)?;
        while let Some(member) = run.next_member() {
            let response = match pool
                .fetch_proxy_registry_path_with_query(
                    &member.name,
                    "-/v1/search",
                    query.as_deref(),
                    Method::GET,
                    upstream_headers(headers),
                    FetchOperation::Metadata,
                )
                .await
            {
                Ok(response) => response,
                Err(error) if run.is_group() && source_error_is_group_fallback(&error) => {
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let value = serde_json::from_slice::<Value>(&response.body)
                .map_err(|_| NpmError::InvalidSearchResponse);
            let Ok(value) = value else {
                run.record_failure(&member.name, "invalid search response", false);
                continue;
            };
            if !value.get("objects").is_some_and(Value::is_array)
                || !value.get("total").is_some_and(Value::is_number)
            {
                run.record_failure(&member.name, "invalid search response", false);
                continue;
            }
            self.record(
                &self.event("search_read"),
                entry_name,
                "-/v1/search",
                Some(&response.source_name),
                "miss",
                None,
            );
            return Ok(response.body);
        }
        Err(NpmError::AllSourcesFailed {
            package: "-/v1/search".to_owned(),
            failures: run.failure_summary(),
        })
    }
}
