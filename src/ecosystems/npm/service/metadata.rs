use super::*;

pub(super) struct FetchedMetadata {
    pub(super) key: NpmCacheKey,
    pub(super) canonical_body: Vec<u8>,
    pub(super) content_digest: String,
    pub(super) source_fingerprint: String,
    pub(super) rendered_body: Vec<u8>,
    pub(super) descriptors: Vec<NpmArtifactDescriptor>,
}

impl NpmService {
    pub(super) async fn metadata(
        &self,
        entry_name: &str,
        package: &str,
        pool: &SourcePool,
        base_url: &Url,
    ) -> Result<Vec<u8>, NpmError> {
        if let Some(snapshot) = self.storage.read_metadata(entry_name, package)? {
            if is_allowed_source(pool, entry_name, &snapshot.key.selected_proxy) {
                if !metadata_is_stale(snapshot.validated_at_unix_secs) {
                    if let Ok(body) =
                        self.render_cached_metadata(entry_name, package, &snapshot, pool, base_url)
                    {
                        self.record(
                            &self.event("metadata_read"),
                            entry_name,
                            package,
                            Some(&snapshot.key.selected_proxy),
                            "hit",
                            None,
                        );
                        return Ok(body);
                    }
                    self.storage.remove_metadata(entry_name, package)?;
                } else {
                    match self
                        .fetch_metadata(
                            entry_name,
                            package,
                            pool,
                            Some(&snapshot.key.selected_proxy),
                            base_url,
                        )
                        .await
                    {
                        Ok(fetched) => {
                            if fetched.content_digest == snapshot.content_digest
                                && fetched.key.selected_proxy == snapshot.key.selected_proxy
                                && fetched.source_fingerprint == snapshot.source_fingerprint
                            {
                                self.storage.touch_metadata(
                                    entry_name,
                                    package,
                                    &snapshot.key,
                                    &snapshot.content_digest,
                                    &snapshot.source_fingerprint,
                                )?;
                                self.record(
                                    &self.event("metadata_revalidated"),
                                    entry_name,
                                    package,
                                    Some(&snapshot.key.selected_proxy),
                                    "hit",
                                    None,
                                );
                                if let Ok(body) = self.render_cached_metadata(
                                    entry_name, package, &snapshot, pool, base_url,
                                ) {
                                    return Ok(body);
                                }
                                self.storage.remove_metadata(entry_name, package)?;
                            } else {
                                self.publish_fetched_metadata(entry_name, package, &fetched)?;
                                return Ok(fetched.rendered_body);
                            }
                        }
                        Err(error) if is_not_found_npm_error(&error) => {
                            self.storage.remove_metadata(entry_name, package)?;
                            self.record(
                                &self.event("metadata_revalidate_failed"),
                                entry_name,
                                package,
                                Some(&snapshot.key.selected_proxy),
                                "failed",
                                Some("not_found"),
                            );
                            return Err(error);
                        }
                        Err(_) => {
                            self.record(
                                &self.event("metadata_stale"),
                                entry_name,
                                package,
                                Some(&snapshot.key.selected_proxy),
                                "stale",
                                Some("source_failed"),
                            );
                            if let Ok(body) = self.render_cached_metadata(
                                entry_name, package, &snapshot, pool, base_url,
                            ) {
                                return Ok(body);
                            }
                            self.storage.remove_metadata(entry_name, package)?;
                        }
                    }
                }
            } else {
                self.storage.remove_metadata(entry_name, package)?;
            }
        }

        let fetched = self
            .fetch_metadata(entry_name, package, pool, None, base_url)
            .await?;
        self.publish_fetched_metadata(entry_name, package, &fetched)?;
        Ok(fetched.rendered_body)
    }

    async fn fetch_metadata(
        &self,
        entry_name: &str,
        package: &str,
        pool: &SourcePool,
        preferred_source: Option<&str>,
        base_url: &Url,
    ) -> Result<FetchedMetadata, NpmError> {
        let request_key = (self.flavor.upstream_package_path)(package);
        let mut run = GroupRun::new(pool, entry_name, preferred_source)?;
        while let Some(member) = run.next_member() {
            let response = match pool
                .fetch_proxy_path(&member.name, &request_key, FetchOperation::Metadata)
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
                        package,
                        &member.name,
                        source_failure_category(&error),
                    );
                    continue;
                }
                Err(error) => {
                    self.record_metadata_source_failure(
                        entry_name,
                        package,
                        &member.name,
                        source_failure_category(&error),
                    );
                    return Err(error.into());
                }
            };
            let metadata = match crate::ecosystems::npm::metadata::parse_compatible_metadata(
                &response.body,
                package,
                self.flavor.is_valid_package,
            ) {
                Ok(metadata) => metadata,
                Err(error) => {
                    run.record_failure(&member.name, &error.to_string(), false);
                    self.record_metadata_source_failure(
                        entry_name,
                        package,
                        &member.name,
                        "protocol_invalid",
                    );
                    continue;
                }
            };
            let mut rewritten = match crate::ecosystems::npm::metadata::rewrite_compatible_metadata(
                &metadata,
                base_url,
                self.flavor.ecosystem,
                self.flavor.route,
                entry_name,
                &response.source_name,
                self.flavor.is_valid_package,
            ) {
                Ok(rewritten) => rewritten,
                Err(error) => {
                    run.record_failure(&member.name, &error.to_string(), false);
                    self.record_metadata_source_failure(
                        entry_name,
                        package,
                        &member.name,
                        "protocol_invalid",
                    );
                    continue;
                }
            };
            for descriptor in &mut rewritten.descriptors {
                descriptor.source_fingerprint = member.fingerprint();
            }
            if !descriptors_match_source(&rewritten.descriptors, &member) {
                run.record_failure(
                    &member.name,
                    "metadata tarball URL is outside the configured source",
                    false,
                );
                self.record_metadata_source_failure(
                    entry_name,
                    package,
                    &member.name,
                    "protocol_invalid",
                );
                continue;
            }
            let rendered_body = serde_json::to_vec(&rewritten.body)
                .map_err(|error| NpmError::Metadata(NpmMetadataError::InvalidJson(error)))?;
            let content_digest = sha256(&response.body);
            return Ok(FetchedMetadata {
                key: NpmCacheKey {
                    ecosystem: self.flavor.ecosystem.to_owned(),
                    entry: entry_name.to_owned(),
                    protocol_variant: "npm-compatible-registry".to_owned(),
                    package: package.to_owned(),
                    version: "catalog".to_owned(),
                    representation: "metadata".to_owned(),
                    selected_proxy: response.source_name,
                },
                canonical_body: response.body,
                content_digest,
                source_fingerprint: member.fingerprint(),
                rendered_body,
                descriptors: rewritten.descriptors,
            });
        }

        self.record(
            &self.event("metadata_fetch_failed"),
            entry_name,
            package,
            None,
            "failed",
            Some(if run.all_not_found() {
                "not_found"
            } else {
                "source_failed"
            }),
        );
        if run.all_not_found() {
            return Err(NpmError::Source(run.all_sources_failed(request_key)));
        }
        Err(NpmError::AllSourcesFailed {
            package: package.to_owned(),
            failures: run.failure_summary(),
        })
    }

    fn render_cached_metadata(
        &self,
        entry_name: &str,
        package: &str,
        snapshot: &StoredMetadata,
        pool: &SourcePool,
        base_url: &Url,
    ) -> Result<Vec<u8>, NpmError> {
        let metadata = crate::ecosystems::npm::metadata::parse_compatible_metadata(
            &snapshot.body,
            package,
            self.flavor.is_valid_package,
        )?;
        let mut rewritten = crate::ecosystems::npm::metadata::rewrite_compatible_metadata(
            &metadata,
            base_url,
            self.flavor.ecosystem,
            self.flavor.route,
            entry_name,
            &snapshot.key.selected_proxy,
            self.flavor.is_valid_package,
        )?;
        let member = pool
            .proxy_members(entry_name)
            .and_then(|members| {
                members
                    .into_iter()
                    .find(|member| member.name == snapshot.key.selected_proxy)
            })
            .ok_or_else(|| {
                NpmError::Source(SourcePoolError::UnknownEntry(
                    snapshot.key.selected_proxy.clone(),
                ))
            })?;
        for descriptor in &mut rewritten.descriptors {
            descriptor.source_fingerprint = member.fingerprint();
        }
        if !descriptors_match_source(&rewritten.descriptors, &member) {
            return Err(NpmError::Metadata(NpmMetadataError::InvalidTarballUrl(
                package.to_owned(),
            )));
        }
        if snapshot.source_fingerprint != member.fingerprint() {
            return Err(NpmError::Metadata(NpmMetadataError::InvalidTarballUrl(
                package.to_owned(),
            )));
        }
        self.storage
            .materialize_descriptors(entry_name, &rewritten.descriptors)?;
        serde_json::to_vec(&rewritten.body)
            .map_err(|error| NpmError::Metadata(NpmMetadataError::InvalidJson(error)))
    }

    fn publish_fetched_metadata(
        &self,
        entry_name: &str,
        package: &str,
        fetched: &FetchedMetadata,
    ) -> Result<(), NpmError> {
        self.storage.publish_metadata(
            &fetched.key,
            &fetched.canonical_body,
            &fetched.descriptors,
            &fetched.source_fingerprint,
        )?;
        self.record(
            &self.event("metadata_published"),
            entry_name,
            package,
            Some(&fetched.key.selected_proxy),
            "published",
            None,
        );
        Ok(())
    }
}
