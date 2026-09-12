use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use toml::{Table, Value};
use url::Url;

use super::{DistPath, RustupError};

#[derive(Debug)]
pub struct ManifestPolicy {
    upstream_bases: Vec<Url>,
    source_names: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactSource {
    pub checksum: String,
    pub source_name: String,
}

#[derive(Debug)]
pub struct RewrittenManifest {
    pub manifest: String,
    pub checksum: String,
    pub artifact_sources: BTreeMap<DistPath, ArtifactSource>,
}

impl ManifestPolicy {
    pub fn new(upstream_bases: Vec<Url>) -> Self {
        let source_names = upstream_bases.iter().map(Url::to_string).collect();
        Self {
            upstream_bases,
            source_names,
        }
    }

    pub fn with_sources(upstream_sources: Vec<(String, Url)>) -> Self {
        let (source_names, upstream_bases) = upstream_sources.into_iter().unzip();
        Self {
            upstream_bases,
            source_names,
        }
    }

    fn artifact_source(&self, source: &Url) -> Result<(DistPath, &str), RustupError> {
        if source.query().is_some() || source.fragment().is_some() || !source.username().is_empty()
        {
            return Err(RustupError::UnconfiguredManifestUrl(source.to_string()));
        }

        for (index, base) in self.upstream_bases.iter().enumerate() {
            if !same_origin(base, source) {
                continue;
            }

            let base_path = base.path().trim_end_matches('/');
            let dist_prefix = format!("{base_path}/dist/");
            if let Some(relative_path) = source.path().strip_prefix(&dist_prefix) {
                return DistPath::parse(relative_path).map(|path| {
                    (
                        path,
                        self.source_names
                            .get(index)
                            .expect("manifest policy source name must match base URL")
                            .as_str(),
                    )
                });
            }
        }

        Err(RustupError::UnconfiguredManifestUrl(source.to_string()))
    }
}

pub fn rewrite_manifest(
    source: &str,
    policy: &ManifestPolicy,
    root_url: &Url,
    channel: &str,
) -> Result<RewrittenManifest, RustupError> {
    let mut manifest: Value = toml::from_str(source)?;
    let mut artifact_sources = BTreeMap::new();
    rewrite_value(
        &mut manifest,
        policy,
        root_url,
        channel,
        &mut artifact_sources,
    )?;

    let manifest = toml::to_string(&manifest)?;
    let checksum = format!("{:x}", Sha256::digest(manifest.as_bytes()));

    Ok(RewrittenManifest {
        manifest,
        checksum,
        artifact_sources,
    })
}

fn rewrite_value(
    value: &mut Value,
    policy: &ManifestPolicy,
    root_url: &Url,
    channel: &str,
    artifact_sources: &mut BTreeMap<DistPath, ArtifactSource>,
) -> Result<(), RustupError> {
    match value {
        Value::Table(table) => rewrite_table(table, policy, root_url, channel, artifact_sources),
        Value::Array(values) => {
            for item in values {
                rewrite_value(item, policy, root_url, channel, artifact_sources)?;
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
    channel: &str,
    artifact_sources: &mut BTreeMap<DistPath, ArtifactSource>,
) -> Result<(), RustupError> {
    for (_, value) in table.iter_mut() {
        rewrite_value(value, policy, root_url, channel, artifact_sources)?;
    }

    let url_fields: Vec<String> = table
        .keys()
        .filter(|field| *field == "url" || field.ends_with("_url"))
        .cloned()
        .collect();

    for url_field in url_fields {
        rewrite_url_field(
            table,
            &url_field,
            policy,
            root_url,
            channel,
            artifact_sources,
        )?;
    }

    Ok(())
}

fn rewrite_url_field(
    table: &mut Table,
    url_field: &str,
    policy: &ManifestPolicy,
    root_url: &Url,
    channel: &str,
    artifact_sources: &mut BTreeMap<DistPath, ArtifactSource>,
) -> Result<(), RustupError> {
    let source_url = table
        .get(url_field)
        .and_then(Value::as_str)
        .ok_or_else(|| RustupError::InvalidUrlField(url_field.to_owned()))?;
    let source_url = Url::parse(source_url)
        .map_err(|_| RustupError::UnconfiguredManifestUrl(source_url.to_owned()))?;
    let hash_fields = url_field_to_hash_fields(url_field);
    let (hash_field, hash) = hash_fields
        .iter()
        .find_map(|field| {
            table
                .get(field)
                .and_then(Value::as_str)
                .map(|hash| (field, hash))
        })
        .ok_or_else(|| {
            RustupError::MissingArtifactHash(url_field.to_owned(), hash_fields.join(" or "))
        })?;

    if !is_sha256(hash) {
        return Err(RustupError::InvalidArtifactHash(hash_field.clone()));
    }

    let (path, source_name) = policy.artifact_source(&source_url)?;
    let artifact_source = ArtifactSource {
        checksum: hash.to_owned(),
        source_name: source_name.to_owned(),
    };
    if let Some(existing) = artifact_sources.insert(path.clone(), artifact_source)
        && existing.checksum != hash
    {
        return Err(RustupError::ConflictingArtifactHash(path.to_string()));
    }

    table.insert(
        url_field.to_owned(),
        Value::String(root_dist_url(root_url, channel, &path).to_string()),
    );

    Ok(())
}

fn url_field_to_hash_fields(url_field: &str) -> Vec<String> {
    match url_field.strip_suffix("_url") {
        Some(prefix) => vec![format!("{prefix}_hash"), format!("{prefix}_hash-sha256")],
        None => vec!["hash".to_owned(), "hash-sha256".to_owned()],
    }
}

fn root_dist_url(root_url: &Url, channel: &str, path: &DistPath) -> Url {
    let mut url = root_url.clone();
    let base_path = root_url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}/dist/{channel}/{}", path.as_str()));
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
