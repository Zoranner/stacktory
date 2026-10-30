use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use url::Url;

use crate::{
    SourceConfig,
    upstream::{FetchOperation, GroupRun, SourcePool, source_error_is_not_found},
};

use super::{
    super::{
        WingetAuditDetails, WingetError,
        cache::{self, InstallerBinding},
        group_fallback_source,
        manifest::{RewrittenManifest, rewrite_manifest},
        path::{is_safe_package_id, is_safe_version},
    },
    WingetService, metadata_headers, search_headers,
};

impl WingetService {
    pub(crate) async fn information(
        &self,
        entry_name: &str,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, WingetError> {
        let mut run = GroupRun::new(pool, entry_name, None)?;
        while let Some(member) = run.next_member() {
            let response = match pool
                .fetch_proxy_path_with_headers(
                    &member.name,
                    "information",
                    metadata_headers(false),
                    FetchOperation::Metadata,
                )
                .await
            {
                Ok(response) => response,
                Err(error) if group_fallback_source(&error, pool, entry_name) => {
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    self.record_source_failure(
                        "winget_information_source_failed",
                        "information",
                        entry_name,
                        &member.name,
                        "information",
                        &error.into(),
                    )?;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match rewrite_information(&response.body, entry_name) {
                Ok(body) => {
                    self.record_object(
                        "winget_information_read",
                        WingetAuditDetails {
                            object_key: "information",
                            entry_name,
                            selected_proxy: &member.name,
                            object_type: "information",
                            cache_status: "published",
                            failure_category: None,
                        },
                    )?;
                    return Ok(body);
                }
                Err(error) if run.is_group() => {
                    run.record_failure(&member.name, &error.to_string(), false);
                    self.record_source_failure(
                        "winget_information_source_failed",
                        "information",
                        entry_name,
                        &member.name,
                        "information",
                        &error,
                    )?;
                }
                Err(error) => {
                    self.record_object(
                        "winget_information_read",
                        WingetAuditDetails {
                            object_key: "information",
                            entry_name,
                            selected_proxy: &member.name,
                            object_type: "information",
                            cache_status: "failed",
                            failure_category: Some("invalid_information"),
                        },
                    )?;
                    return Err(error);
                }
            }
        }
        let failure_category = if run.all_not_found() {
            "not_found"
        } else {
            "source_failed"
        };
        let error = run.all_sources_failed("information");
        self.record_object(
            "winget_information_fetch_failed",
            WingetAuditDetails {
                object_key: "information",
                entry_name,
                selected_proxy: "",
                object_type: "information",
                cache_status: "failed",
                failure_category: Some(failure_category),
            },
        )?;
        Err(error.into())
    }

    pub(crate) async fn search(
        &self,
        entry_name: &str,
        body: &[u8],
        pool: &SourcePool,
    ) -> Result<Vec<u8>, WingetError> {
        validate_search_request(body)?;
        let mut run = GroupRun::new(pool, entry_name, None)?;
        while let Some(member) = run.next_member() {
            let response = match pool
                .post_json_to_source_with_headers(
                    &member.name,
                    "manifestSearch",
                    body,
                    search_headers(),
                )
                .await
            {
                Ok(response) => response,
                Err(error) if group_fallback_source(&error, pool, entry_name) => {
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    self.record_source_failure(
                        "winget_search_source_failed",
                        "manifestSearch",
                        entry_name,
                        &member.name,
                        "search",
                        &error.into(),
                    )?;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if let Err(error) = validate_search_response(&response.body) {
                if run.is_group() {
                    run.record_failure(&member.name, &error.to_string(), false);
                    self.record_source_failure(
                        "winget_search_source_failed",
                        "manifestSearch",
                        entry_name,
                        &member.name,
                        "search",
                        &error,
                    )?;
                    continue;
                }
                return Err(error);
            }
            self.record_object(
                "winget_search_query",
                WingetAuditDetails {
                    object_key: "manifestSearch",
                    entry_name,
                    selected_proxy: &member.name,
                    object_type: "search",
                    cache_status: "published",
                    failure_category: None,
                },
            )?;
            return Ok(response.body);
        }
        let failure_category = if run.all_not_found() {
            "not_found"
        } else {
            "source_failed"
        };
        let error = run.all_sources_failed("manifestSearch");
        self.record_object(
            "winget_search_fetch_failed",
            WingetAuditDetails {
                object_key: "manifestSearch",
                entry_name,
                selected_proxy: "",
                object_type: "search",
                cache_status: "failed",
                failure_category: Some(failure_category),
            },
        )?;
        Err(error.into())
    }

    pub(crate) async fn manifest(
        &self,
        entry_name: &str,
        package_id: &str,
        query: Option<&str>,
        base_url: &Url,
        pool: &SourcePool,
    ) -> Result<Vec<u8>, WingetError> {
        let request_key = format!("packageManifests/{package_id}");
        let mut run = GroupRun::new(pool, entry_name, None)?;
        while let Some(member) = run.next_member() {
            let manifest = match self
                .manifest_from_source(entry_name, &member, package_id, query, base_url, pool)
                .await
            {
                Ok(manifest) => manifest,
                Err(WingetError::Source(error))
                    if group_fallback_source(&error, pool, entry_name) =>
                {
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    self.record_source_failure(
                        "winget_manifest_source_failed",
                        &request_key,
                        entry_name,
                        &member.name,
                        "manifest",
                        &WingetError::Source(error),
                    )?;
                    continue;
                }
                Err(error) if run.is_group() => {
                    let not_found = matches!(error, WingetError::VersionNotFound { .. });
                    run.record_failure(&member.name, &error.to_string(), not_found);
                    self.record_source_failure(
                        "winget_manifest_source_failed",
                        &request_key,
                        entry_name,
                        &member.name,
                        "manifest",
                        &error,
                    )?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let fingerprint = pool
                .source_fingerprint(entry_name, &member.name)
                .ok_or_else(|| {
                    WingetError::InvalidManifest("source fingerprint is missing".to_owned())
                })?;
            for installer in &manifest.installers {
                let binding = InstallerBinding {
                    package_id: installer.package_id.clone(),
                    version: installer.version.clone(),
                    installer_token: installer.installer_token.clone(),
                    installer_sha256: installer.installer_sha256.clone(),
                    installer_type: installer.installer_type.clone(),
                    architecture: installer.architecture.clone(),
                    scope: installer.scope.clone(),
                    installer_locale: installer.installer_locale.clone(),
                    installer_index: installer.installer_index,
                    request_query: installer.request_query.clone(),
                    selected_proxy: member.name.clone(),
                    source_fingerprint: fingerprint.clone(),
                    upstream_url: installer.upstream_url.clone(),
                    content_type: None,
                    validated_at_unix_secs: now_unix_secs(),
                };
                cache::write_binding(&self.data_dir, entry_name, &binding)?;
            }
            self.record_object(
                "winget_manifest_read",
                WingetAuditDetails {
                    object_key: &request_key,
                    entry_name,
                    selected_proxy: &member.name,
                    object_type: "manifest",
                    cache_status: "published",
                    failure_category: None,
                },
            )?;
            return Ok(manifest.body);
        }
        let failure_category = if run.all_not_found() {
            "not_found"
        } else {
            "source_failed"
        };
        let error = run.all_sources_failed(&request_key);
        self.record_object(
            "winget_manifest_fetch_failed",
            WingetAuditDetails {
                object_key: &request_key,
                entry_name,
                selected_proxy: "",
                object_type: "manifest",
                cache_status: "failed",
                failure_category: Some(failure_category),
            },
        )?;
        Err(error.into())
    }

    pub(crate) async fn manifest_from_source(
        &self,
        entry_name: &str,
        source: &SourceConfig,
        package_id: &str,
        query: Option<&str>,
        base_url: &Url,
        pool: &SourcePool,
    ) -> Result<RewrittenManifest, WingetError> {
        let allowed_hosts = self.installer_hosts(&source.name).ok_or_else(|| {
            WingetError::InstallerPolicy(format!(
                "source `{}` has no installer host allowlist",
                source.name
            ))
        })?;
        let response = pool
            .fetch_proxy_path_with_query_and_headers(
                &source.name,
                &format!("packageManifests/{package_id}"),
                query,
                metadata_headers(true),
                FetchOperation::Metadata,
            )
            .await?;
        rewrite_manifest(
            &response.body,
            entry_name,
            package_id,
            query,
            allowed_hosts,
            source.require_https,
            base_url,
        )
    }
}

fn rewrite_information(source: &[u8], entry_name: &str) -> Result<Vec<u8>, WingetError> {
    let mut value: Value = serde_json::from_slice(source)?;
    let data = value
        .get_mut("Data")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| WingetError::InvalidInformation("Data is missing".to_owned()))?;
    required_information_string(data, "SourceIdentifier")?;
    let versions = data
        .get("ServerSupportedVersions")
        .and_then(Value::as_array)
        .filter(|versions| !versions.is_empty())
        .ok_or_else(|| {
            WingetError::InvalidInformation("ServerSupportedVersions is missing".to_owned())
        })?;
    let mut supports_1_0 = false;
    for version in versions {
        let version = version.as_str().ok_or_else(|| {
            WingetError::InvalidInformation(
                "ServerSupportedVersions contains a non-string".to_owned(),
            )
        })?;
        supports_1_0 |= supports_contract_1_0(version);
    }
    if !supports_1_0 {
        return Err(WingetError::InvalidInformation(
            "upstream does not support REST contract 1.0".to_owned(),
        ));
    }
    validate_source_agreements(data)?;
    validate_authentication(data)?;
    data.insert(
        "SourceIdentifier".to_owned(),
        Value::String(format!("stacktory-winget:{entry_name}")),
    );
    data.insert(
        "ServerSupportedVersions".to_owned(),
        serde_json::json!(["1.0.0"]),
    );
    Ok(serde_json::to_vec(&value)?)
}

fn required_information_string(object: &Map<String, Value>, name: &str) -> Result<(), WingetError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| WingetError::InvalidInformation(format!("{name} is missing")))?;
    Ok(())
}

fn supports_contract_1_0(value: &str) -> bool {
    let mut parts = value.split('.');
    parts.next().and_then(|part| part.parse::<u64>().ok()) == Some(1)
        && parts.next().and_then(|part| part.parse::<u64>().ok()) == Some(0)
}

fn validate_source_agreements(data: &Map<String, Value>) -> Result<(), WingetError> {
    let Some(agreements) = data
        .get("SourceAgreements")
        .filter(|value| !value.is_null())
    else {
        return Ok(());
    };
    let agreements = agreements.as_object().ok_or_else(|| {
        WingetError::InvalidInformation("SourceAgreements is not an object".to_owned())
    })?;
    agreements
        .get("AgreementsIdentifier")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            WingetError::InvalidInformation(
                "SourceAgreements.AgreementsIdentifier is missing".to_owned(),
            )
        })?;
    let Some(entries) = agreements.get("Agreements") else {
        return Ok(());
    };
    let entries = entries.as_array().ok_or_else(|| {
        WingetError::InvalidInformation("SourceAgreements.Agreements is not an array".to_owned())
    })?;
    for entry in entries {
        let entry = entry.as_object().ok_or_else(|| {
            WingetError::InvalidInformation("source agreement is not an object".to_owned())
        })?;
        for name in ["AgreementLabel", "Agreement", "AgreementUrl"] {
            if let Some(value) = entry.get(name)
                && !value.is_null()
                && !value.is_string()
            {
                return Err(WingetError::InvalidInformation(format!(
                    "source agreement {name} is not a string"
                )));
            }
        }
    }
    Ok(())
}

fn validate_authentication(data: &Map<String, Value>) -> Result<(), WingetError> {
    let Some(authentication) = data.get("Authentication").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let authentication = authentication.as_object().ok_or_else(|| {
        WingetError::InvalidInformation("Authentication is not an object".to_owned())
    })?;
    let authentication_type = authentication
        .get("AuthenticationType")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            WingetError::InvalidInformation("AuthenticationType is missing".to_owned())
        })?;
    if authentication_type.eq_ignore_ascii_case("none") {
        return Ok(());
    }
    Err(WingetError::UnsupportedAuthentication(
        authentication_type.to_owned(),
    ))
}

fn validate_search_request(body: &[u8]) -> Result<(), WingetError> {
    let value: Value = serde_json::from_slice(body).map_err(|error| {
        WingetError::InvalidRequest(format!("manifestSearch body is not JSON: {error}"))
    })?;
    if !value.is_object() {
        return Err(WingetError::InvalidRequest(
            "manifestSearch body is not a JSON object".to_owned(),
        ));
    }
    Ok(())
}

fn validate_search_response(body: &[u8]) -> Result<(), WingetError> {
    let value: Value = serde_json::from_slice(body)?;
    let data = value
        .get("Data")
        .and_then(Value::as_array)
        .ok_or_else(|| WingetError::InvalidSearch("Data is missing".to_owned()))?;
    for package in data {
        let package = package.as_object().ok_or_else(|| {
            WingetError::InvalidSearch("package entry is not an object".to_owned())
        })?;
        let package_id = required_search_string(package, "PackageIdentifier")?;
        if !is_safe_package_id(package_id) {
            return Err(WingetError::InvalidSearch(
                "PackageIdentifier is not path safe".to_owned(),
            ));
        }
        required_search_string(package, "PackageName")?;
        required_search_string(package, "Publisher")?;
        let versions = package
            .get("Versions")
            .and_then(Value::as_array)
            .filter(|versions| !versions.is_empty())
            .ok_or_else(|| WingetError::InvalidSearch("Versions is missing".to_owned()))?;
        for version in versions {
            let version = version.as_object().ok_or_else(|| {
                WingetError::InvalidSearch("version entry is not an object".to_owned())
            })?;
            let version = required_search_string(version, "PackageVersion")?;
            if !is_safe_version(version) {
                return Err(WingetError::InvalidSearch(
                    "PackageVersion is not path safe".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn required_search_string<'a>(
    object: &'a Map<String, Value>,
    name: &str,
) -> Result<&'a str, WingetError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| WingetError::InvalidSearch(format!("{name} is missing")))
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
