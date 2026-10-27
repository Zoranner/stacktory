use std::{fs, path::PathBuf};

use serde_json::Value;
use url::Url;

use super::{NugetError, NugetService, ServiceIndexDescriptor, ServiceIndexSnapshot};
use crate::{SourcePool, ecosystems::nuget::now_unix_secs, storage::atomic::atomic_write};

const REGISTRATIONS_BASE_URL: &str = "RegistrationsBaseUrl";
const PACKAGE_BASE_ADDRESS: &str = "PackageBaseAddress";

impl NugetService {
    pub(super) fn read_cached_service_index(
        &self,
        entry_name: &str,
        pool: &SourcePool,
    ) -> Result<Option<(Vec<u8>, ServiceIndexDescriptor)>, NugetError> {
        let Some(body) = read_if_present(&self.service_index_path(entry_name))? else {
            return Ok(None);
        };
        let Some(descriptor) = read_descriptor(&self.service_index_descriptor_path(entry_name))?
        else {
            return Ok(None);
        };
        let valid = !descriptor.public_base_url.is_empty()
            && is_valid_cached_service_index(&body, &descriptor)
            && pool
                .source_fingerprint(entry_name, &descriptor.selected_proxy)
                .is_some_and(|fingerprint| fingerprint == descriptor.source_fingerprint);
        Ok(valid.then_some((body, descriptor)))
    }

    pub(super) fn write_service_index_body(
        &self,
        entry_name: &str,
        body: &[u8],
    ) -> Result<(), NugetError> {
        atomic_write(&self.service_index_path(entry_name), body)?;
        Ok(())
    }

    pub(super) fn write_service_index_descriptor(
        &self,
        entry_name: &str,
        descriptor: &ServiceIndexDescriptor,
    ) -> Result<(), NugetError> {
        atomic_write(
            &self.service_index_descriptor_path(entry_name),
            &serde_json::to_vec(descriptor)?,
        )?;
        Ok(())
    }

    pub(super) fn write_service_index_cache(
        &self,
        entry_name: &str,
        body: &[u8],
        descriptor: &ServiceIndexDescriptor,
    ) -> Result<(), NugetError> {
        self.write_service_index_body(entry_name, body)?;
        self.write_service_index_descriptor(entry_name, descriptor)
    }

    pub(super) fn remove_service_index_cache(&self, entry_name: &str) {
        let _ = fs::remove_file(self.service_index_path(entry_name));
        let _ = fs::remove_file(self.service_index_descriptor_path(entry_name));
    }

    pub(crate) fn publish_service_index(
        &self,
        entry_name: &str,
        snapshot: &ServiceIndexSnapshot,
        base_url: &Url,
    ) -> Result<(), NugetError> {
        let descriptor = ServiceIndexDescriptor {
            selected_proxy: snapshot.selected_proxy.clone(),
            source_fingerprint: snapshot.source_fingerprint.clone(),
            public_base_url: base_url.to_string(),
            validated_at_unix_secs: now_unix_secs(),
            registration_url: snapshot.registration_url.as_ref().map(Url::to_string),
            registration_urls: snapshot
                .registration_urls
                .iter()
                .map(Url::to_string)
                .collect(),
            package_base_url: snapshot.package_base_url.as_ref().map(Url::to_string),
            search_url: snapshot.search_url.as_ref().map(Url::to_string),
            vulnerability_url: snapshot.vulnerability_url.as_ref().map(Url::to_string),
        };
        self.write_service_index_cache(entry_name, &snapshot.body, &descriptor)
    }

    fn service_index_path(&self, entry_name: &str) -> PathBuf {
        crate::storage::layout::metadata_root(&self.data_dir, "nuget")
            .join(entry_name)
            .join("service-index.json")
    }

    fn service_index_descriptor_path(&self, entry_name: &str) -> PathBuf {
        crate::storage::layout::metadata_root(&self.data_dir, "nuget")
            .join(entry_name)
            .join("service-index.source")
    }
}

pub(super) fn snapshot_from_descriptor(
    body: Vec<u8>,
    descriptor: ServiceIndexDescriptor,
) -> Result<ServiceIndexSnapshot, NugetError> {
    let registration_url = descriptor.registration_url.clone();
    Ok(ServiceIndexSnapshot {
        body,
        selected_proxy: descriptor.selected_proxy,
        source_fingerprint: descriptor.source_fingerprint,
        registration_url: parse_optional_url(registration_url.clone())?,
        registration_urls: parse_url_list(descriptor.registration_urls, registration_url)?,
        package_base_url: parse_optional_url(descriptor.package_base_url)?,
        search_url: parse_optional_url(descriptor.search_url)?,
        vulnerability_url: parse_optional_url(descriptor.vulnerability_url)?,
    })
}

fn read_descriptor(path: &std::path::Path) -> Result<Option<ServiceIndexDescriptor>, NugetError> {
    let Some(contents) = read_if_present(path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(NugetError::Serialization)
}

fn parse_optional_url(value: Option<String>) -> Result<Option<Url>, NugetError> {
    value
        .map(|value| {
            Url::parse(&value)
                .map_err(|_| NugetError::InvalidServiceIndex("stored URL is invalid".to_owned()))
        })
        .transpose()
}

fn parse_url_list(values: Vec<String>, fallback: Option<String>) -> Result<Vec<Url>, NugetError> {
    let values = if values.is_empty() {
        fallback.into_iter().collect()
    } else {
        values
    };
    values
        .into_iter()
        .map(|value| {
            Url::parse(&value)
                .map_err(|_| NugetError::InvalidServiceIndex("stored URL is invalid".to_owned()))
        })
        .collect()
}

fn is_valid_cached_service_index(body: &[u8], descriptor: &ServiceIndexDescriptor) -> bool {
    descriptor.registration_url.is_some()
        && descriptor.package_base_url.is_some()
        && serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|index| index.get("resources").and_then(Value::as_array).cloned())
            .is_some_and(|resources| {
                resources.iter().any(|resource| {
                    resource
                        .get("@type")
                        .and_then(Value::as_str)
                        .is_some_and(|resource_type| {
                            resource_type.starts_with(REGISTRATIONS_BASE_URL)
                        })
                }) && resources.iter().any(|resource| {
                    resource
                        .get("@type")
                        .and_then(Value::as_str)
                        .is_some_and(|resource_type| {
                            resource_type.starts_with(PACKAGE_BASE_ADDRESS)
                        })
                })
            })
}

fn read_if_present(path: &std::path::Path) -> Result<Option<Vec<u8>>, std::io::Error> {
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
