use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NpmPackagePath {
    pub package: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NpmArtifactPath {
    pub artifact_key: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum NpmPathError {
    #[error("npm path `{0}` is invalid")]
    InvalidPath(String),
    #[error("npm package name `{0}` is invalid")]
    InvalidPackageName(String),
    #[error("npm artifact key `{0}` is invalid")]
    InvalidArtifactKey(String),
}

pub fn parse_metadata_path(path: &str) -> Result<NpmPackagePath, NpmPathError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('?')
        || path.contains('#')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(NpmPathError::InvalidPath(path.to_owned()));
    }

    // Percent-encoded separators are case-insensitive on the wire. Bun emits
    // `%2f` for scoped packages while some npm clients use `%2F`.
    let normalized_path;
    let path = if path.contains("%2f") {
        normalized_path = path.replace("%2f", "%2F");
        normalized_path.as_str()
    } else {
        path
    };

    let package = if let Some((scope, package)) = path.split_once("%2F") {
        if path.matches("%2F").count() != 1 || scope.contains('%') || package.contains('%') {
            return Err(NpmPathError::InvalidPath(path.to_owned()));
        }
        let scope = scope
            .strip_prefix('@')
            .ok_or_else(|| NpmPathError::InvalidPackageName(scope.to_owned()))?;
        validate_name_part(scope)?;
        validate_name_part(package)?;
        format!("@{scope}/{package}")
    } else {
        if path.contains('%') {
            return Err(NpmPathError::InvalidPath(path.to_owned()));
        }
        validate_name_part(path)?;
        path.to_owned()
    };

    Ok(NpmPackagePath { package })
}

pub(crate) fn validate_package_name(value: &str) -> Result<(), NpmPathError> {
    if let Some((scope, package)) = value
        .strip_prefix('@')
        .and_then(|value| value.split_once('/'))
    {
        if value.matches('/').count() != 1 {
            return Err(NpmPathError::InvalidPackageName(value.to_owned()));
        }
        validate_name_part(scope)?;
        validate_name_part(package)?;
    } else {
        validate_name_part(value)?;
    }
    Ok(())
}

pub fn parse_artifact_path(path: &str) -> Result<NpmArtifactPath, NpmPathError> {
    if path.len() != 64
        || !path.bytes().all(|byte| byte.is_ascii_hexdigit())
        || path.chars().any(char::is_control)
    {
        return Err(NpmPathError::InvalidArtifactKey(path.to_owned()));
    }
    Ok(NpmArtifactPath {
        artifact_key: path.to_owned(),
    })
}

fn validate_name_part(value: &str) -> Result<(), NpmPathError> {
    if value.is_empty()
        || value.len() > 214
        || value.starts_with('.')
        || value.starts_with('_')
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(NpmPathError::InvalidPackageName(value.to_owned()));
    }
    Ok(())
}
