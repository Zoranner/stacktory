//! 项目索引：HTML 解析、规范对象发布、HTML/JSON 表示渲染与来源重验证。

use axum::response::Response;
use url::Url;

use crate::upstream::{
    FetchOperation, GroupRun, SourcePoolError, source_error_is_group_fallback,
    source_error_is_not_found,
};

use super::{
    MAX_PROJECT_INDEX_BYTES, PyPiError, base_url_fingerprint_for, hash_fragment, html_headers,
    internal_file_url, is_html_content_type, metadata_is_stale, now_unix_secs,
    representation_response, source_fingerprint_matches, upstream_project_url,
};
use crate::ecosystems::pypi::{
    accept::PyPiRepresentation,
    cache::{
        PyPiFileBinding, PyPiMetadataKey, PyPiRepresentationKey, StoredProjectIndex, sha256_hex,
    },
    index::{PyPiIndex, PyPiIndexError, parse_project_index, render_html, render_json},
};

struct FetchedProject {
    key: PyPiMetadataKey,
    body: Vec<u8>,
    index: PyPiIndex,
    source_fingerprint: String,
}

impl FetchedProject {
    fn into_stored(self) -> StoredProjectIndex {
        StoredProjectIndex {
            key: self.key,
            canonical_digest: sha256_hex(&self.body),
            body: self.body,
            index: self.index,
            validated_at_unix_secs: now_unix_secs(),
            source_fingerprint: self.source_fingerprint,
        }
    }
}

impl super::PyPiService {
    pub(super) async fn project_index(
        &self,
        entry_name: &str,
        project: &str,
        representation: PyPiRepresentation,
        pool: &crate::SourcePool,
        base_url: &Url,
    ) -> Response {
        let snapshot = match self.project_index_snapshot(entry_name, project, pool).await {
            Ok(snapshot) => snapshot,
            Err(error) => return self.error_response(&error),
        };
        let key = PyPiRepresentationKey {
            entry: entry_name.to_owned(),
            project: project.to_owned(),
            selected_proxy: snapshot.key.selected_proxy.clone(),
            canonical_digest: snapshot.canonical_digest.clone(),
            base_url_fingerprint: base_url_fingerprint_for(base_url),
            representation,
        };
        match self.storage.read_representation(&key) {
            Ok(Some(body)) => {
                self.record(
                    &self.event("metadata_read"),
                    entry_name,
                    project,
                    Some(&key.selected_proxy),
                    "hit",
                    None,
                );
                return representation_response(representation, body);
            }
            Ok(None) => {}
            Err(error) => return self.error_response(&PyPiError::Storage(error)),
        }
        let body = match render_representation(&snapshot, representation, base_url, entry_name) {
            Ok(body) => body,
            Err(error) => return self.error_response(&error),
        };
        if let Err(error) = self.storage.publish_representation(&key, &body) {
            return self.error_response(&PyPiError::Storage(error));
        }
        self.record(
            "pypi_metadata_published_representation",
            entry_name,
            project,
            Some(&key.selected_proxy),
            "published",
            None,
        );
        representation_response(representation, body)
    }
}
impl super::PyPiService {
    pub(super) async fn project_index_snapshot(
        &self,
        entry_name: &str,
        project: &str,
        pool: &crate::SourcePool,
    ) -> Result<StoredProjectIndex, PyPiError> {
        let cached = self.storage.read_metadata(entry_name, project)?;
        if let Some(snapshot) = cached {
            let source_valid = source_fingerprint_matches(
                pool,
                entry_name,
                &snapshot.key.selected_proxy,
                &snapshot.source_fingerprint,
            );
            if source_valid && !metadata_is_stale(snapshot.validated_at_unix_secs) {
                self.record(
                    "pypi_metadata_read",
                    entry_name,
                    project,
                    Some(&snapshot.key.selected_proxy),
                    "hit",
                    None,
                );
                return Ok(snapshot);
            }
            match self
                .fetch_project(
                    entry_name,
                    project,
                    pool,
                    Some(&snapshot.key.selected_proxy),
                )
                .await
            {
                Ok(fetched) => {
                    if fetched.body == snapshot.body
                        && fetched.key.selected_proxy == snapshot.key.selected_proxy
                        && fetched.source_fingerprint == snapshot.source_fingerprint
                    {
                        self.storage.touch_metadata(
                            &snapshot.key,
                            &snapshot.canonical_digest,
                            &snapshot.source_fingerprint,
                        )?;
                        let mut touched = snapshot;
                        touched.validated_at_unix_secs = now_unix_secs();
                        self.record(
                            "pypi_metadata_revalidated",
                            entry_name,
                            project,
                            Some(&touched.key.selected_proxy),
                            "hit",
                            None,
                        );
                        return Ok(touched);
                    }
                    self.publish_fetched_project(entry_name, project, fetched)
                }
                Err(PyPiError::MissingProject(_)) => {
                    self.storage.remove_metadata(entry_name, project)?;
                    self.record(
                        "pypi_metadata_revalidate_failed",
                        entry_name,
                        project,
                        Some(&snapshot.key.selected_proxy),
                        "failed",
                        Some("not_found"),
                    );
                    Err(PyPiError::MissingProject(project.to_owned()))
                }
                Err(error) => {
                    self.record(
                        "pypi_metadata_revalidate_failed",
                        entry_name,
                        project,
                        Some(&snapshot.key.selected_proxy),
                        "failed",
                        Some("source_failed"),
                    );
                    Err(error)
                }
            }
        } else {
            let fetched = self.fetch_project(entry_name, project, pool, None).await?;
            self.publish_fetched_project(entry_name, project, fetched)
        }
    }

    fn publish_fetched_project(
        &self,
        entry_name: &str,
        project: &str,
        fetched: FetchedProject,
    ) -> Result<StoredProjectIndex, PyPiError> {
        self.storage.publish_metadata(
            &fetched.key,
            &fetched.body,
            &fetched.index,
            &fetched.source_fingerprint,
        )?;
        self.record(
            "pypi_metadata_published",
            entry_name,
            project,
            Some(&fetched.key.selected_proxy),
            "published",
            None,
        );
        Ok(fetched.into_stored())
    }
}
impl super::PyPiService {
    async fn fetch_project(
        &self,
        entry_name: &str,
        project: &str,
        pool: &crate::SourcePool,
        preferred_source: Option<&str>,
    ) -> Result<FetchedProject, PyPiError> {
        let request_key = format!("simple/{project}/");
        let mut run = GroupRun::new(pool, entry_name, preferred_source)?;
        while let Some(member) = run.next_member() {
            let Some(base_url) = member.base_url.as_ref() else {
                continue;
            };
            let response = match pool
                .fetch_proxy_path_with_headers_limited(
                    &member.name,
                    &request_key,
                    html_headers(),
                    FetchOperation::Metadata,
                    MAX_PROJECT_INDEX_BYTES,
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
                    self.record_metadata_source_failure(
                        entry_name,
                        project,
                        &member.name,
                        source_failure_category(&error),
                    );
                    continue;
                }
                Err(error) => {
                    self.record_metadata_source_failure(
                        entry_name,
                        project,
                        &member.name,
                        source_failure_category(&error),
                    );
                    return Err(error.into());
                }
            };
            if !is_html_content_type(response.content_type.as_deref()) {
                run.record_failure(&member.name, "unexpected project index content type", false);
                self.record_metadata_source_failure(
                    entry_name,
                    project,
                    &member.name,
                    "protocol_invalid",
                );
                continue;
            }
            let project_url = upstream_project_url(base_url, project);
            let index = match parse_project_index(
                &response.body,
                &project_url,
                base_url,
                member.require_https,
                project,
            ) {
                Ok(index) => index,
                Err(error) => {
                    run.record_failure(&member.name, &error.to_string(), false);
                    self.record_metadata_source_failure(
                        entry_name,
                        project,
                        &member.name,
                        "protocol_invalid",
                    );
                    continue;
                }
            };
            let body = serde_json::to_vec(&index).map_err(|error| {
                PyPiError::Index(PyPiIndexError::Serialization(error.to_string()))
            })?;
            return Ok(FetchedProject {
                key: PyPiMetadataKey {
                    entry: entry_name.to_owned(),
                    project: project.to_owned(),
                    selected_proxy: member.name.clone(),
                },
                body,
                index,
                source_fingerprint: member.fingerprint(),
            });
        }
        self.record(
            "pypi_metadata_fetch_failed",
            entry_name,
            project,
            None,
            "failed",
            Some(if run.all_not_found() {
                "not_found"
            } else {
                "source_failed"
            }),
        );
        if run.all_not_found() {
            return Err(PyPiError::MissingProject(project.to_owned()));
        }
        Err(PyPiError::AllSourcesFailed {
            project: project.to_owned(),
            failures: run.failure_summary(),
        })
    }
}
impl super::PyPiService {
    pub(super) async fn binding_from_source(
        &self,
        _entry_name: &str,
        project: &str,
        filename: &str,
        member: &crate::SourceConfig,
        pool: &crate::SourcePool,
    ) -> Result<PyPiFileBinding, PyPiError> {
        let Some(base_url) = member.base_url.as_ref() else {
            return Err(PyPiError::Source(SourcePoolError::UnknownEntry(
                member.name.clone(),
            )));
        };
        let response = pool
            .fetch_proxy_path_with_headers_limited(
                &member.name,
                &format!("simple/{project}/"),
                html_headers(),
                FetchOperation::Metadata,
                MAX_PROJECT_INDEX_BYTES,
            )
            .await?;
        if !is_html_content_type(response.content_type.as_deref()) {
            return Err(PyPiError::Index(PyPiIndexError::UnexpectedContentType(
                response.content_type.unwrap_or_default(),
            )));
        }
        let project_url = upstream_project_url(base_url, project);
        let index = parse_project_index(
            &response.body,
            &project_url,
            base_url,
            member.require_https,
            project,
        )?;
        let file = index
            .files
            .into_iter()
            .find(|file| file.filename == filename)
            .ok_or_else(|| PyPiError::MissingFile(filename.to_owned()))?;
        Ok(PyPiFileBinding::from_file(
            project,
            &member.name,
            &member.fingerprint(),
            &file,
        ))
    }
}

fn render_representation(
    snapshot: &StoredProjectIndex,
    representation: PyPiRepresentation,
    base_url: &Url,
    entry_name: &str,
) -> Result<Vec<u8>, PyPiError> {
    let project = snapshot.index.name.clone();
    match representation {
        PyPiRepresentation::Html => Ok(render_html(&snapshot.index, |file| {
            internal_file_url(
                base_url,
                entry_name,
                &project,
                &file.filename,
                hash_fragment(&file.hashes).as_deref(),
            )
        })
        .into_bytes()),
        PyPiRepresentation::Json => render_json(&snapshot.index, |file| {
            internal_file_url(base_url, entry_name, &project, &file.filename, None)
        })
        .map_err(|error| PyPiError::Index(PyPiIndexError::Serialization(error.to_string()))),
    }
}

fn source_failure_category(error: &SourcePoolError) -> &'static str {
    if source_error_is_not_found(error) {
        "not_found"
    } else {
        "source_failed"
    }
}
