//! 项目文件：绑定校验、selected proxy 优先、group 回退和原子发布。

use axum::response::Response;
use url::Url;

use crate::upstream::{
    FetchOperation, GroupRun, source_error_is_group_fallback, source_error_is_not_found,
};

use super::{PyPiError, artifact_response, source_fingerprint_matches};
use crate::ecosystems::pypi::{
    cache::{ArtifactFile, PyPiArtifactKey, PyPiFileBinding, StorageError},
    index::PyPiIndexError,
};

impl super::PyPiService {
    pub(super) async fn file_response(
        &self,
        entry_name: &str,
        project: &str,
        filename: &str,
        pool: &crate::SourcePool,
    ) -> Response {
        match self
            .file_artifact(entry_name, project, filename, pool)
            .await
        {
            Ok(file) => artifact_response(file).await,
            Err(error) => self.error_response(&error),
        }
    }

    async fn file_artifact(
        &self,
        entry_name: &str,
        project: &str,
        filename: &str,
        pool: &crate::SourcePool,
    ) -> Result<ArtifactFile, PyPiError> {
        let mut binding = self
            .storage
            .read_binding(entry_name, project, filename)?
            .ok_or_else(|| PyPiError::MissingFile(filename.to_owned()))?;
        if binding.project != project || binding.filename != filename {
            return Err(PyPiError::MissingFile(filename.to_owned()));
        }
        if !source_fingerprint_matches(
            pool,
            entry_name,
            &binding.selected_proxy,
            &binding.source_fingerprint,
        ) {
            self.project_index_snapshot(entry_name, project, pool)
                .await?;
            binding = self
                .storage
                .read_binding(entry_name, project, filename)?
                .ok_or_else(|| PyPiError::MissingFile(filename.to_owned()))?;
            if binding.project != project || binding.filename != filename {
                return Err(PyPiError::MissingFile(filename.to_owned()));
            }
            if !source_fingerprint_matches(
                pool,
                entry_name,
                &binding.selected_proxy,
                &binding.source_fingerprint,
            ) {
                return Err(PyPiError::MissingFile(filename.to_owned()));
            }
        }

        let declared_hashes = binding.hashes.clone();
        let mut run = GroupRun::new(pool, entry_name, Some(&binding.selected_proxy))?;
        let mut had_integrity_failure = false;
        while let Some(member) = run.next_member() {
            let candidate: PyPiFileBinding = if member.name == binding.selected_proxy {
                binding.clone()
            } else {
                match self
                    .binding_from_source(entry_name, project, filename, &member, pool)
                    .await
                {
                    Ok(candidate) => candidate,
                    Err(error)
                        if run.is_group()
                            && is_retryable_source_error(&error, pool, entry_name) =>
                    {
                        run.record_failure(
                            &member.name,
                            &error.to_string(),
                            is_not_found_error(&error),
                        );
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            };

            let expected_hashes = if declared_hashes.is_empty() {
                &candidate.hashes
            } else {
                &declared_hashes
            };
            let key = PyPiArtifactKey {
                entry: entry_name.to_owned(),
                project: project.to_owned(),
                selected_proxy: member.name.clone(),
                filename: filename.to_owned(),
            };
            if let Some(file) = self.storage.read_verified_artifact(&key, expected_hashes)? {
                if member.name != binding.selected_proxy {
                    let updated = rebound_binding(
                        &candidate,
                        project,
                        filename,
                        &member.name,
                        &declared_hashes,
                    );
                    self.storage.write_binding(entry_name, &updated)?;
                    self.record(
                        "pypi_artifact_fallback",
                        entry_name,
                        filename,
                        Some(&member.name),
                        "hit",
                        Some("integrity_mismatch"),
                    )?;
                }
                self.record(
                    "pypi_artifact_read",
                    entry_name,
                    filename,
                    Some(&member.name),
                    "hit",
                    None,
                )?;
                return Ok(file);
            }

            let upstream_url = Url::parse(&candidate.upstream_url).map_err(|_| {
                PyPiError::Index(PyPiIndexError::InvalidLink(candidate.upstream_url.clone()))
            })?;
            let temporary_path = self.storage.temporary_artifact_path()?;
            let fetched = match pool
                .fetch_configured_url_following_safe_redirects_to_file(
                    &member.name,
                    upstream_url,
                    FetchOperation::Artifact,
                    temporary_path.clone(),
                )
                .await
            {
                Ok(file) => file,
                Err(error) if run.is_group() && source_error_is_group_fallback(&error) => {
                    let _ = std::fs::remove_file(&temporary_path);
                    crate::logging::error(format!(
                        "PyPI artifact `{filename}` from `{}` failed: {error}",
                        member.name
                    ));
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    continue;
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&temporary_path);
                    crate::logging::error(format!(
                        "PyPI artifact `{filename}` from `{}` failed: {error}",
                        member.name
                    ));
                    return Err(error.into());
                }
            };
            match self
                .storage
                .publish_verified_artifact_file(&key, expected_hashes, &fetched.path)
            {
                Ok(file) => {
                    if member.name == binding.selected_proxy {
                        self.record(
                            "pypi_artifact_published",
                            entry_name,
                            filename,
                            Some(&member.name),
                            "published",
                            None,
                        )?;
                    } else {
                        let updated = rebound_binding(
                            &candidate,
                            project,
                            filename,
                            &member.name,
                            &declared_hashes,
                        );
                        self.storage.write_binding(entry_name, &updated)?;
                        self.record(
                            "pypi_artifact_fallback",
                            entry_name,
                            filename,
                            Some(&member.name),
                            "published",
                            Some("integrity_mismatch"),
                        )?;
                    }
                    if expected_hashes.is_empty() {
                        self.record(
                            "pypi_artifact_published_without_digest",
                            entry_name,
                            filename,
                            Some(&member.name),
                            "published",
                            None,
                        )?;
                    }
                    return Ok(file);
                }
                Err(StorageError::IntegrityMismatch(_)) => {
                    had_integrity_failure = true;
                    run.record_failure(&member.name, "integrity mismatch", false);
                    self.record(
                        "pypi_artifact_source_failed",
                        entry_name,
                        filename,
                        Some(&member.name),
                        "failed",
                        Some("integrity_mismatch"),
                    )?;
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }

        self.record(
            "pypi_artifact_fetch_failed",
            entry_name,
            filename,
            None,
            "failed",
            Some(if had_integrity_failure {
                "integrity_mismatch"
            } else {
                "source_failed"
            }),
        )?;
        if run.all_not_found() {
            return Err(PyPiError::MissingFile(filename.to_owned()));
        }
        Err(PyPiError::AllSourcesFailed {
            project: project.to_owned(),
            failures: run.failure_summary(),
        })
    }
}

fn rebound_binding(
    candidate: &PyPiFileBinding,
    project: &str,
    filename: &str,
    selected_proxy: &str,
    declared_hashes: &std::collections::BTreeMap<String, String>,
) -> PyPiFileBinding {
    let mut updated = candidate.clone();
    updated.project = project.to_owned();
    updated.filename = filename.to_owned();
    updated.selected_proxy = selected_proxy.to_owned();
    if !declared_hashes.is_empty() {
        updated.hashes = declared_hashes.clone();
    }
    updated
}

fn is_retryable_source_error(
    error: &PyPiError,
    pool: &crate::SourcePool,
    entry_name: &str,
) -> bool {
    match error {
        PyPiError::MissingFile(_) | PyPiError::Index(_) => true,
        PyPiError::Source(error) => {
            pool.is_group(entry_name) && source_error_is_group_fallback(error)
        }
        _ => false,
    }
}

fn is_not_found_error(error: &PyPiError) -> bool {
    match error {
        PyPiError::MissingFile(_) => true,
        PyPiError::Source(error) => source_error_is_not_found(error),
        _ => false,
    }
}
