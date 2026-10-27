use super::super::{RewrittenManifest, ServiceError};
use super::RustupService;
use crate::upstream::{
    FetchOperation, GroupRun, SourcePoolError, source_error_is_group_fallback,
    source_error_is_not_found,
};

impl RustupService {
    pub(super) async fn fetch_and_rewrite_manifest(
        &self,
        source_name: &str,
        channel: &str,
    ) -> Result<(String, RewrittenManifest), ServiceError> {
        let mut run = GroupRun::new(&self.sources, source_name, None)?;
        let is_group = run.is_group();
        let manifest_key = format!("dist/channel-rust-{channel}.toml");
        let checksum_key = format!("{manifest_key}.sha256");
        let entry_root = self.entry_root_url(self.public_urls.primary(), source_name);
        let file_name = format!("channel-rust-{channel}.toml");
        let policy = self.manifest_policy();
        while let Some(member) = run.next_member() {
            let manifest = match self
                .sources
                .fetch(&member.name, &manifest_key, FetchOperation::Metadata)
                .await
            {
                Ok(response) => response.body,
                Err(error) if should_try_next_source(&error, is_group) => {
                    self.record_manifest_member_failure(
                        source_name,
                        channel,
                        &member.name,
                        source_failure_category(&error),
                    );
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let checksum_file = match self
                .sources
                .fetch(&member.name, &checksum_key, FetchOperation::Metadata)
                .await
            {
                Ok(response) => response.body,
                Err(error) if should_try_next_source(&error, is_group) => {
                    self.record_manifest_member_failure(
                        source_name,
                        channel,
                        &member.name,
                        source_failure_category(&error),
                    );
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let manifest = match String::from_utf8(manifest) {
                Ok(manifest) => manifest,
                Err(_) => {
                    self.record_manifest_member_failure(
                        source_name,
                        channel,
                        &member.name,
                        "protocol_invalid",
                    );
                    run.record_failure(&member.name, "manifest is not UTF-8", false);
                    continue;
                }
            };
            let checksum_file = match String::from_utf8(checksum_file) {
                Ok(checksum_file) => checksum_file,
                Err(_) => {
                    self.record_manifest_member_failure(
                        source_name,
                        channel,
                        &member.name,
                        "protocol_invalid",
                    );
                    run.record_failure(&member.name, "checksum is not UTF-8", false);
                    continue;
                }
            };
            let actual_checksum = sha256(manifest.as_bytes());
            if parse_checksum_file(&checksum_file, &file_name) != Some(actual_checksum.as_str()) {
                self.record_manifest_member_failure(
                    source_name,
                    channel,
                    &member.name,
                    "checksum_mismatch",
                );
                run.record_failure(&member.name, "checksum does not match manifest", false);
                continue;
            }
            match crate::ecosystems::rustup::rewrite_manifest(
                &manifest,
                &policy,
                &entry_root,
                channel,
            ) {
                Ok(rewritten) => return Ok((member.name, rewritten)),
                Err(error) => {
                    self.record_manifest_member_failure(
                        source_name,
                        channel,
                        &member.name,
                        "protocol_invalid",
                    );
                    run.record_failure(&member.name, &error.to_string(), false);
                }
            }
        }
        Err(run.all_sources_failed(manifest_key).into())
    }
}

fn should_try_next_source(error: &SourcePoolError, is_group: bool) -> bool {
    matches!(error, SourcePoolError::AllSourcesFailed { .. })
        || (is_group && source_error_is_group_fallback(error))
}

fn parse_checksum_file<'a>(contents: &'a str, expected_file_name: &str) -> Option<&'a str> {
    let mut fields = contents.split_whitespace();
    let checksum = fields.next()?;
    let file_name = fields.next()?.trim_start_matches('*');
    if fields.next().is_some()
        || file_name != expected_file_name
        || checksum.len() != 64
        || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(checksum)
}

fn sha256(contents: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    format!("{:x}", Sha256::digest(contents))
}

fn source_failure_category(error: &SourcePoolError) -> &'static str {
    match error {
        SourcePoolError::TerminalSourceError { status: 404, .. }
        | SourcePoolError::AllSourcesFailed {
            status: Some(404), ..
        } => "not_found",
        _ => "source_failed",
    }
}
