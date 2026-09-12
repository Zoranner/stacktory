use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpmPackagePath {
    pub package: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum UpmPathError {
    #[error("UPM package path `{0}` is invalid")]
    InvalidPath(String),
    #[error("UPM package name `{0}` is invalid")]
    InvalidPackageName(String),
}

pub fn parse_metadata_path(path: &str) -> Result<UpmPackagePath, UpmPathError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('%')
        || path.contains('?')
        || path.contains('#')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(UpmPathError::InvalidPath(path.to_owned()));
    }
    validate_package_name(path)?;
    Ok(UpmPackagePath {
        package: path.to_owned(),
    })
}

pub fn validate_package_name(value: &str) -> Result<(), UpmPathError> {
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() < 3
        || parts.iter().any(|part| {
            part.is_empty()
                || part.starts_with('-')
                || part.ends_with('-')
                || !part.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_')
                })
        })
    {
        return Err(UpmPathError::InvalidPackageName(value.to_owned()));
    }
    Ok(())
}
