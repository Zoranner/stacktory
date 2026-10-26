use axum::{
    body::Body,
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use super::*;

pub(super) fn query_object_key(request_key: &str, query: Option<&str>) -> String {
    match query {
        Some(query) if !query.is_empty() => format!("{request_key}?{query}"),
        _ => request_key.to_owned(),
    }
}

#[derive(Debug)]
pub(super) struct SearchQuery {
    pub(super) term: String,
    pub(super) limit: usize,
}

pub(super) fn validate_list_query(query: Option<&str>) -> Result<Option<String>, DockerError> {
    let Some(query) = query else {
        return Ok(None);
    };
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "n" => {
                let limit = value
                    .parse::<usize>()
                    .map_err(|_| DockerError::InvalidSearchQuery)?;
                if !(1..=1000).contains(&limit) {
                    return Err(DockerError::InvalidSearchQuery);
                }
            }
            "last" if !value.is_empty() => {}
            _ => return Err(DockerError::InvalidSearchQuery),
        }
    }
    Ok(Some(query.to_owned()))
}

pub(super) fn parse_search_query(query: Option<&str>) -> Result<SearchQuery, DockerError> {
    let query = query.ok_or(DockerError::InvalidSearchQuery)?;
    let mut term = None;
    let mut limit = 25;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "q" if term.is_none() && !value.is_empty() => {
                if value.chars().any(char::is_control) {
                    return Err(DockerError::InvalidSearchQuery);
                }
                term = Some(value.into_owned());
            }
            "n" if limit == 25 => {
                limit = value
                    .parse::<usize>()
                    .map_err(|_| DockerError::InvalidSearchQuery)?;
                if !(1..=100).contains(&limit) {
                    return Err(DockerError::InvalidSearchQuery);
                }
            }
            _ => return Err(DockerError::InvalidSearchQuery),
        }
    }
    Ok(SearchQuery {
        term: term.ok_or(DockerError::InvalidSearchQuery)?,
        limit,
    })
}

pub(super) fn catalog_page_query(last: Option<&str>) -> String {
    let mut query = url::form_urlencoded::Serializer::new("n=1000".to_owned());
    if let Some(last) = last {
        query.append_pair("last", last);
    }
    query.finish()
}

pub(super) fn next_catalog_last(headers: &HeaderMap) -> Option<String> {
    let link = headers.get(header::LINK)?.to_str().ok()?;
    let target = link
        .split(',')
        .find(|value| value.contains("rel=\"next\""))?
        .trim();
    let target = target.strip_prefix('<')?.split_once('>')?.0;
    let query = target.split_once('?')?.1;
    url::form_urlencoded::parse(query.as_bytes())
        .find_map(|(key, value)| (key == "last").then(|| value.into_owned()))
}

pub(super) fn validate_list_response(value: &Value, object_type: &str) -> Result<(), DockerError> {
    match object_type {
        "catalog" => {
            let repositories = value
                .get("repositories")
                .and_then(Value::as_array)
                .ok_or(DockerError::InvalidCatalog)?;
            if repositories.iter().all(|value| value.as_str().is_some()) {
                Ok(())
            } else {
                Err(DockerError::InvalidCatalog)
            }
        }
        "tags" => {
            let name = value.get("name").and_then(Value::as_str);
            let tags = value.get("tags").and_then(Value::as_array);
            if name.is_some()
                && tags.is_some_and(|tags| tags.iter().all(|value| value.as_str().is_some()))
            {
                Ok(())
            } else {
                Err(DockerError::InvalidTags)
            }
        }
        _ => Err(DockerError::InvalidManifest(
            "unknown Docker query response type".to_owned(),
        )),
    }
}

pub(super) fn json_response(
    body: Vec<u8>,
    content_type: Option<&str>,
    link: Option<&str>,
) -> Response {
    let mut response = (StatusCode::OK, Body::from(body)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(content_type.unwrap_or("application/json"))
            .unwrap_or_else(|_| header::HeaderValue::from_static("application/json")),
    );
    if let Some(link) = link
        && let Ok(value) = header::HeaderValue::from_str(link)
    {
        response.headers_mut().insert(header::LINK, value);
    }
    response
}

impl DockerService {
    pub(super) async fn catalog(
        &self,
        entry_name: &str,
        query: Option<&str>,
        pool: &SourcePool,
    ) -> Result<Response, DockerError> {
        let query = validate_list_query(query)?;
        self.list_endpoint(
            entry_name,
            "v2/_catalog",
            "_catalog",
            query.as_deref(),
            "catalog",
            pool,
        )
        .await
    }

    pub(super) async fn tags(
        &self,
        entry_name: &str,
        repository: &str,
        query: Option<&str>,
        pool: &SourcePool,
    ) -> Result<Response, DockerError> {
        let query = validate_list_query(query)?;
        self.list_endpoint(
            entry_name,
            &format!("v2/{repository}/tags/list"),
            repository,
            query.as_deref(),
            "tags",
            pool,
        )
        .await
    }

    async fn list_endpoint(
        &self,
        entry_name: &str,
        request_key: &str,
        namespace: &str,
        query: Option<&str>,
        object_type: &str,
        pool: &SourcePool,
    ) -> Result<Response, DockerError> {
        let result = self
            .query_result(entry_name, request_key, namespace, query, object_type, pool)
            .await?;
        Ok(json_response(
            result.body,
            result.content_type.as_deref(),
            result.link.as_deref(),
        ))
    }

    async fn query_result(
        &self,
        entry_name: &str,
        request_key: &str,
        namespace: &str,
        query: Option<&str>,
        object_type: &str,
        pool: &SourcePool,
    ) -> Result<QueryResult, DockerError> {
        let bound = read_query_metadata(
            &self.data_dir,
            entry_name,
            object_type,
            namespace,
            request_key,
            query,
        )?
        .filter(|metadata| {
            source_is_current(
                pool,
                entry_name,
                &metadata.selected_proxy,
                &metadata.source_fingerprint,
            )
        });
        if let Some(metadata) = &bound
            && let Some(body) = read_verified_artifact(
                &self.data_dir,
                &metadata.selected_proxy,
                object_type,
                &metadata.digest,
            )?
        {
            self.record(
                "docker_query_read",
                DockerAuditDetails {
                    entry_name,
                    object_key: &query_object_key(request_key, query),
                    selected_proxy: &metadata.selected_proxy,
                    object_type,
                    cache_status: "hit",
                    failure_category: None,
                },
            );
            return Ok(QueryResult {
                body,
                content_type: metadata.content_type.clone(),
                link: metadata.link.clone(),
            });
        }

        let mut run = GroupRun::new(
            pool,
            entry_name,
            bound
                .as_ref()
                .map(|metadata| metadata.selected_proxy.as_str()),
        )?;
        while let Some(member) = run.next_member() {
            let source_name = member.name;
            match pool
                .fetch_proxy_registry_path_with_query(
                    &source_name,
                    request_key,
                    query,
                    Method::GET,
                    HeaderMap::new(),
                    FetchOperation::Metadata,
                )
                .await
            {
                Ok(upstream) => {
                    let validation = serde_json::from_slice::<Value>(&upstream.body)
                        .map_err(|_| {
                            DockerError::InvalidManifest(format!("{object_type} is not valid JSON"))
                        })
                        .and_then(|value| validate_list_response(&value, object_type));
                    if let Err(error) = validation {
                        if run.is_group() && is_fallback_search_error(&error) {
                            run.record_failure(&source_name, &error.to_string(), false);
                            continue;
                        }
                        return Err(error);
                    }
                    let digest = sha256_digest(&upstream.body);
                    let metadata = QueryMetadata {
                        namespace: namespace.to_owned(),
                        request_key: request_key.to_owned(),
                        query: query.map(str::to_owned),
                        object_type: object_type.to_owned(),
                        selected_proxy: source_name.clone(),
                        source_fingerprint: pool
                            .source_fingerprint(entry_name, &source_name)
                            .unwrap_or_default(),
                        digest: digest.clone(),
                        content_type: upstream.content_type.clone(),
                        link: upstream
                            .headers
                            .get(header::LINK)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned),
                    };
                    write_verified_artifact(
                        &self.data_dir,
                        &source_name,
                        object_type,
                        &digest,
                        &upstream.body,
                    )?;
                    write_query_metadata(&self.data_dir, entry_name, &metadata)?;
                    self.record(
                        "docker_query_read",
                        DockerAuditDetails {
                            entry_name,
                            object_key: &query_object_key(request_key, query),
                            selected_proxy: &source_name,
                            object_type,
                            cache_status: "miss",
                            failure_category: None,
                        },
                    );
                    return Ok(QueryResult {
                        body: upstream.body,
                        content_type: metadata.content_type,
                        link: metadata.link,
                    });
                }
                Err(error) if run.is_group() && is_fallback_source_error(&error) => {
                    run.record_failure(
                        &source_name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(DockerError::Source(
            run.all_sources_failed(request_key.to_owned()),
        ))
    }

    pub(super) async fn search(
        &self,
        entry_name: &str,
        query: Option<&str>,
        pool: &SourcePool,
    ) -> Result<Response, DockerError> {
        let query = parse_search_query(query)?;
        let mut run = GroupRun::new(pool, entry_name, None)?;
        while let Some(member) = run.next_member() {
            let source_name = member.name;
            match self.search_source(&source_name, &query, pool).await {
                Ok(results) => {
                    let body = serde_json::to_vec(&json!({
                        "query": query.term,
                        "results": results,
                    }))?;
                    return Ok(json_response(body, Some("application/json"), None));
                }
                Err(error) if run.is_group() && is_fallback_search_error(&error) => {
                    run.record_failure(&source_name, &error.to_string(), false);
                }
                Err(error) => return Err(error),
            }
        }
        Err(DockerError::Source(SourcePoolError::AllSourcesFailed {
            entry_name: entry_name.to_owned(),
            request_key: "v2/_catalog".to_owned(),
            failures: run.failure_summary(),
            status: None,
        }))
    }

    async fn search_source(
        &self,
        source_name: &str,
        query: &SearchQuery,
        pool: &SourcePool,
    ) -> Result<Vec<Value>, DockerError> {
        let mut last = None;
        let mut results = Vec::new();
        let needle = query.term.to_ascii_lowercase();
        for _ in 0..100 {
            let page_query = catalog_page_query(last.as_deref());
            let upstream = pool
                .fetch_proxy_registry_path_with_query(
                    source_name,
                    "v2/_catalog",
                    Some(&page_query),
                    Method::GET,
                    HeaderMap::new(),
                    FetchOperation::Metadata,
                )
                .await?;
            let value: Value =
                serde_json::from_slice(&upstream.body).map_err(|_| DockerError::InvalidCatalog)?;
            validate_list_response(&value, "catalog")?;
            for repository in value["repositories"]
                .as_array()
                .expect("validated catalog repositories")
            {
                let repository = repository.as_str().ok_or(DockerError::InvalidCatalog)?;
                if repository.to_ascii_lowercase().contains(&needle) {
                    results.push(json!({ "name": repository }));
                    if results.len() >= query.limit {
                        return Ok(results);
                    }
                }
            }
            let Some(next_last) = next_catalog_last(&upstream.headers) else {
                return Ok(results);
            };
            if last.as_deref() == Some(next_last.as_str()) {
                return Err(DockerError::InvalidCatalog);
            }
            last = Some(next_last);
        }
        Err(DockerError::InvalidCatalog)
    }
}
