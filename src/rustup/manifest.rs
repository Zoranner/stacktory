use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use toml::{Table, Value};
use url::Url;

use super::{DistPath, RustupError};

#[derive(Debug)]
pub struct ManifestPolicy {
    upstream_bases: Vec<Url>,
}

#[derive(Debug)]
pub struct RewrittenManifest {
    pub manifest: String,
    pub checksum: String,
    pub artifact_hashes: BTreeMap<DistPath, String>,
}

impl ManifestPolicy {
    pub fn new(upstream_bases: Vec<Url>) -> Self {
        Self { upstream_bases }
    }

    fn dist_path(&self, source: &Url) -> Result<DistPath, RustupError> {
        if source.query().is_some() || source.fragment().is_some() || !source.username().is_empty()
        {
            return Err(RustupError::UnconfiguredManifestUrl(source.to_string()));
        }

        for base in &self.upstream_bases {
            if !same_origin(base, source) {
                continue;
            }

            let base_path = base.path().trim_end_matches('/');
            let dist_prefix = format!("{base_path}/dist/");
            if let Some(relative_path) = source.path().strip_prefix(&dist_prefix) {
                return DistPath::parse(relative_path);
            }
        }

        Err(RustupError::UnconfiguredManifestUrl(source.to_string()))
    }
}

pub fn rewrite_manifest(
    source: &str,
    policy: &ManifestPolicy,
    root_url: &Url,
) -> Result<RewrittenManifest, RustupError> {
    let mut manifest: Value = toml::from_str(source)?;
    let mut artifact_hashes = BTreeMap::new();
    rewrite_value(&mut manifest, policy, root_url, &mut artifact_hashes)?;

    let manifest = toml::to_string(&manifest)?;
    let checksum = format!("{:x}", Sha256::digest(manifest.as_bytes()));

    Ok(RewrittenManifest {
        manifest,
        checksum,
        artifact_hashes,
    })
}

fn rewrite_value(
    value: &mut Value,
    policy: &ManifestPolicy,
    root_url: &Url,
    artifact_hashes: &mut BTreeMap<DistPath, String>,
) -> Result<(), RustupError> {
    match value {
        Value::Table(table) => rewrite_table(table, policy, root_url, artifact_hashes),
        Value::Array(values) => {
            for item in values {
                rewrite_value(item, policy, root_url, artifact_hashes)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn rewrite_table(
    table: &mut Table,
    policy: &ManifestPolicy,
    root_url: &Url,
    artifact_hashes: &mut BTreeMap<DistPath, String>,
) -> Result<(), RustupError> {
    for (_, value) in table.iter_mut() {
        rewrite_value(value, policy, root_url, artifact_hashes)?;
    }

    let url_fields: Vec<String> = table
        .keys()
        .filter(|field| *field == "url" || field.ends_with("_url"))
        .cloned()
        .collect();

    for url_field in url_fields {
        rewrite_url_field(table, &url_field, policy, root_url, artifact_hashes)?;
    }

    Ok(())
}

fn rewrite_url_field(
    table: &mut Table,
    url_field: &str,
    policy: &ManifestPolicy,
    root_url: &Url,
    artifact_hashes: &mut BTreeMap<DistPath, String>,
) -> Result<(), RustupError> {
    let source_url = table
        .get(url_field)
        .and_then(Value::as_str)
        .ok_or_else(|| RustupError::InvalidUrlField(url_field.to_owned()))?;
    let source_url = Url::parse(source_url)
        .map_err(|_| RustupError::UnconfiguredManifestUrl(source_url.to_owned()))?;
    let hash_field = url_field_to_hash_field(url_field);
    let hash = table
        .get(&hash_field)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RustupError::MissingArtifactHash(url_field.to_owned(), hash_field.clone())
        })?;

    if !is_sha256(hash) {
        return Err(RustupError::InvalidArtifactHash(hash_field));
    }

    let path = policy.dist_path(&source_url)?;
    if let Some(existing_hash) = artifact_hashes.insert(path.clone(), hash.to_owned())
        && existing_hash != hash
    {
        return Err(RustupError::ConflictingArtifactHash(path.to_string()));
    }

    table.insert(
        url_field.to_owned(),
        Value::String(root_dist_url(root_url, &path).to_string()),
    );

    Ok(())
}

fn url_field_to_hash_field(url_field: &str) -> String {
    match url_field.strip_suffix("_url") {
        Some(prefix) => format!("{prefix}_hash"),
        None => "hash".to_owned(),
    }
}

fn root_dist_url(root_url: &Url, path: &DistPath) -> Url {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/dist/{}", path.as_str()));
    url.set_query(None);
    url.set_fragment(None);
    url
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
