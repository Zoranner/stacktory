use super::*;
use crate::ecosystems::npm::path::{NpmTarballPath, tarball_file_name};

impl NpmService {
    pub(super) async fn standard_tarball(
        &self,
        entry_name: &str,
        request: &NpmTarballPath,
        pool: &SourcePool,
    ) -> Result<ArtifactFile, NpmError> {
        if !(self.flavor.is_valid_package)(&request.package) {
            return Err(NpmError::MissingArtifact(request.file_name.clone()));
        }
        let _ = self.metadata(entry_name, &request.package, pool).await?;
        let snapshot = self
            .storage
            .read_metadata(entry_name, &request.package)?
            .ok_or_else(|| NpmError::MissingArtifact(request.file_name.clone()))?;
        let descriptor = snapshot
            .descriptors
            .iter()
            .find(|descriptor| {
                descriptor.package == request.package
                    && tarball_file_name(&descriptor.package, &descriptor.version)
                        == request.file_name
            })
            .ok_or_else(|| NpmError::MissingArtifact(request.file_name.clone()))?;
        self.tarball(entry_name, &descriptor.artifact_key, pool)
            .await
    }

    pub(super) async fn tarball(
        &self,
        entry_name: &str,
        artifact_key: &str,
        pool: &SourcePool,
    ) -> Result<ArtifactFile, NpmError> {
        let descriptor = self
            .storage
            .read_descriptor(entry_name, artifact_key)?
            .ok_or_else(|| NpmError::MissingArtifact(artifact_key.to_owned()))?;
        if !is_allowed_source(pool, entry_name, &descriptor.selected_proxy) {
            return Err(NpmError::MissingArtifact(artifact_key.to_owned()));
        }
        let selected_member = pool
            .proxy_members(entry_name)
            .and_then(|members| {
                members
                    .into_iter()
                    .find(|member| member.name == descriptor.selected_proxy)
            })
            .ok_or_else(|| NpmError::MissingArtifact(artifact_key.to_owned()))?;
        if descriptor.source_fingerprint != selected_member.fingerprint() {
            return Err(NpmError::MissingArtifact(artifact_key.to_owned()));
        }

        let mut run = GroupRun::new(pool, entry_name, Some(&descriptor.selected_proxy))?;
        while let Some(member) = run.next_member() {
            let candidate = if member.name == descriptor.selected_proxy {
                descriptor.clone()
            } else {
                match self
                    .descriptor_from_source(
                        entry_name,
                        &descriptor.package,
                        &descriptor.version,
                        &member,
                        pool,
                    )
                    .await
                {
                    Ok(candidate) => candidate,
                    Err(error) if is_retryable_npm(&error, pool, entry_name) => {
                        run.record_failure(
                            &member.name,
                            &error.to_string(),
                            is_not_found_npm_error(&error),
                        );
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            };
            let key = NpmCacheKey {
                ecosystem: self.flavor.ecosystem.to_owned(),
                entry: entry_name.to_owned(),
                protocol_variant: "npm-compatible-registry".to_owned(),
                package: candidate.package.clone(),
                version: candidate.version.clone(),
                representation: "tarball".to_owned(),
                selected_proxy: member.name.clone(),
            };
            let expectation = expectation(&candidate);
            if let Some(file) =
                self.storage
                    .read_verified_artifact_file(&key, artifact_key, &expectation)?
            {
                self.record(
                    &self.event("artifact_read"),
                    entry_name,
                    artifact_key,
                    Some(&member.name),
                    "hit",
                    None,
                );
                return Ok(file);
            }
            let upstream_url = Url::parse(&candidate.upstream_tarball).map_err(|_| {
                NpmError::Metadata(NpmMetadataError::InvalidTarballUrl(
                    candidate.upstream_tarball.clone(),
                ))
            })?;
            let temporary_path = self.storage.temporary_artifact_path()?;
            let fetched = match pool
                .fetch_configured_url_following_safe_redirects_to_file(
                    &member.name,
                    upstream_url,
                    FetchOperation::Artifact,
                    temporary_path,
                )
                .await
            {
                Ok(file) => file,
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
            match self.storage.publish_verified_artifact_file(
                &key,
                artifact_key,
                &expectation,
                &fetched.path,
            ) {
                Ok(file) => {
                    self.record(
                        &self.event("artifact_published"),
                        entry_name,
                        artifact_key,
                        Some(&member.name),
                        "published",
                        None,
                    );
                    return Ok(file);
                }
                Err(StorageError::IntegrityMismatch(_)) => {
                    run.record_failure(&member.name, "integrity mismatch", false);
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }

        self.record(
            &self.event("artifact_fetch_failed"),
            entry_name,
            artifact_key,
            None,
            "failed",
            Some("source_failed"),
        );
        Err(NpmError::AllSourcesFailed {
            package: descriptor.package,
            failures: run.failure_summary(),
        })
    }

    async fn descriptor_from_source(
        &self,
        entry_name: &str,
        package: &str,
        version: &str,
        member: &crate::SourceConfig,
        pool: &SourcePool,
    ) -> Result<NpmArtifactDescriptor, NpmError> {
        let response = pool
            .fetch_proxy_path(
                &member.name,
                &(self.flavor.upstream_package_path)(package),
                FetchOperation::Metadata,
            )
            .await?;
        let metadata = crate::ecosystems::npm::metadata::parse_compatible_metadata(
            &response.body,
            package,
            self.flavor.is_valid_package,
        )?;
        let mut rewritten = crate::ecosystems::npm::metadata::rewrite_compatible_metadata(
            &metadata,
            &self.root_url,
            self.flavor.ecosystem,
            self.flavor.route,
            entry_name,
            &member.name,
            self.flavor.is_valid_package,
        )?;
        for descriptor in &mut rewritten.descriptors {
            descriptor.source_fingerprint = member.fingerprint();
        }
        if !descriptors_match_source(&rewritten.descriptors, member) {
            return Err(NpmError::Metadata(NpmMetadataError::InvalidTarballUrl(
                package.to_owned(),
            )));
        }
        rewritten
            .descriptors
            .into_iter()
            .find(|descriptor| descriptor.version == version)
            .ok_or_else(|| NpmError::MissingArtifact(version.to_owned()))
    }
}
