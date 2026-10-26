use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DockerRequest {
    Probe,
    Catalog,
    Tags {
        repository: String,
    },
    Search,
    Manifest {
        repository: String,
        reference: ManifestReference,
    },
    Blob {
        repository: String,
        digest: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestReference {
    Tag(String),
    Digest(String),
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DockerPathError {
    #[error("Docker Registry V2 path `{0}` is invalid")]
    InvalidPath(String),
    #[error("Docker repository `{0}` is invalid")]
    InvalidRepository(String),
    #[error("Docker manifest reference `{0}` is invalid")]
    InvalidReference(String),
    #[error("Docker digest `{0}` is invalid")]
    InvalidDigest(String),
}

pub fn parse_request(path: &str) -> Result<DockerRequest, DockerPathError> {
    if path == "search" {
        return Ok(DockerRequest::Search);
    }
    if path == "v2/" {
        return Ok(DockerRequest::Probe);
    }
    let rest = path
        .strip_prefix("v2/")
        .ok_or_else(|| DockerPathError::InvalidPath(path.to_owned()))?;

    if rest == "_catalog" {
        return Ok(DockerRequest::Catalog);
    }
    if let Some(repository) = rest.strip_suffix("/tags/list") {
        return Ok(DockerRequest::Tags {
            repository: parse_repository(repository)?,
        });
    }

    if let Some((repository, reference)) = rest.split_once("/manifests/") {
        let repository = parse_repository(repository)?;
        let reference = parse_manifest_reference(reference)?;
        return Ok(DockerRequest::Manifest {
            repository,
            reference,
        });
    }

    if let Some((repository, digest)) = rest.split_once("/blobs/") {
        let repository = parse_repository(repository)?;
        let digest = parse_digest(digest)?;
        return Ok(DockerRequest::Blob { repository, digest });
    }

    Err(DockerPathError::InvalidPath(path.to_owned()))
}

pub fn parse_repository(repository: &str) -> Result<String, DockerPathError> {
    if repository.is_empty()
        || repository.starts_with('/')
        || repository.ends_with('/')
        || repository.contains('\\')
        || repository.contains('%')
        || repository.contains('?')
        || repository.contains('#')
        || repository.chars().any(char::is_control)
        || repository
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        || repository.split('/').any(|segment| {
            !segment.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            }) || !segment
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
                || !segment
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
    {
        return Err(DockerPathError::InvalidRepository(repository.to_owned()));
    }
    Ok(repository.to_owned())
}

fn parse_manifest_reference(reference: &str) -> Result<ManifestReference, DockerPathError> {
    if reference.is_empty()
        || reference.contains('/')
        || reference.contains('\\')
        || reference.contains('%')
        || reference.contains('?')
        || reference.contains('#')
        || reference.chars().any(char::is_control)
    {
        return Err(DockerPathError::InvalidReference(reference.to_owned()));
    }
    if reference.starts_with("sha256:") {
        return parse_digest(reference).map(ManifestReference::Digest);
    }
    if reference.len() > 128
        || !reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || !reference
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(DockerPathError::InvalidReference(reference.to_owned()));
    }
    Ok(ManifestReference::Tag(reference.to_owned()))
}

fn parse_digest(digest: &str) -> Result<String, DockerPathError> {
    if digest.len() != "sha256:".len() + 64
        || !digest.starts_with("sha256:")
        || !digest[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(DockerPathError::InvalidDigest(digest.to_owned()));
    }
    Ok(digest.to_owned())
}
