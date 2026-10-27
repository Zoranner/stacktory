use std::collections::BTreeMap;

use serde_json::Value;
use url::Url;

use super::{NugetError, PackageBinding, RegistrationPath};
use crate::ecosystems::nuget::path::is_safe_segment;
use crate::ecosystems::nuget::service_index::ServiceIndexSnapshot;

pub(super) fn rewrite_registration(
    value: &mut Value,
    request: &RegistrationPath,
    snapshot: &ServiceIndexSnapshot,
    entry_name: &str,
    registration_base: &Url,
    bindings: &mut BTreeMap<String, PackageBinding>,
    base_url: &Url,
) -> Result<(), NugetError> {
    let object = value.as_object_mut().ok_or_else(|| {
        NugetError::InvalidRegistration("registration response is not an object".to_owned())
    })?;
    super::rewrite_object(
        object,
        request,
        snapshot,
        entry_name,
        registration_base,
        bindings,
        base_url,
    )
}

pub(super) fn append_relative_path(base_url: &Url, relative: &str) -> Result<Url, NugetError> {
    let mut url = base_url.clone();
    let base_path = url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/{relative}"));
    Ok(url)
}

pub(super) fn package_content_url(
    base_url: &Url,
    package_id: &str,
    version: &str,
) -> Result<Url, NugetError> {
    let mut url = base_url.clone();
    let base_path = url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base_path}/{}/{}/{}.{}.nupkg",
        package_id.to_ascii_lowercase(),
        version.to_ascii_lowercase(),
        package_id.to_ascii_lowercase(),
        version.to_ascii_lowercase()
    ));
    Ok(url)
}

pub(super) fn registration_relative_path(
    base_url: &Url,
    candidate: &Url,
) -> Result<String, NugetError> {
    if !same_origin(base_url, candidate) {
        return Err(NugetError::InvalidRegistration(
            "registration URL is outside the discovered resource".to_owned(),
        ));
    }
    let base_path = base_url.path().trim_end_matches('/');
    let relative = candidate
        .path()
        .strip_prefix(&format!("{base_path}/"))
        .ok_or_else(|| {
            NugetError::InvalidRegistration(
                "registration URL is outside the discovered resource".to_owned(),
            )
        })?;
    if relative.is_empty()
        || relative.contains('%')
        || relative.contains('\\')
        || relative.split('/').any(|segment| !is_safe_segment(segment))
    {
        return Err(NugetError::InvalidRegistration(
            "registration URL contains unsafe path".to_owned(),
        ));
    }
    Ok(relative.to_owned())
}

pub(super) fn local_registration_url(root_url: &Url, entry_name: &str, relative: &str) -> String {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!(
        "{base_path}/mirror/nuget/{entry_name}/registration/{relative}"
    ));
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

pub(super) fn local_package_url(
    root_url: &Url,
    entry_name: &str,
    package_id: &str,
    version: &str,
) -> String {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    let package_id = package_id.to_ascii_lowercase();
    let version = version.to_ascii_lowercase();
    url.set_path(&format!(
        "{base_path}/mirror/nuget/{entry_name}/package/{package_id}/{version}/{package_id}.{version}.nupkg"
    ));
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

pub(super) fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}
