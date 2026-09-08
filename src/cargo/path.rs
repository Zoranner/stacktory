use std::fmt;

use thiserror::Error;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CargoPath(String);

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CargoPathError {
    #[error("cargo path `{0}` is invalid")]
    Invalid(String),
    #[error("crate name `{0}` is invalid")]
    InvalidCrateName(String),
    #[error("crate version `{0}` is invalid")]
    InvalidVersion(String),
}

impl CargoPath {
    pub fn parse(value: &str) -> Result<Self, CargoPathError> {
        if value.is_empty()
            || value.starts_with('/')
            || value.contains('\\')
            || value.contains('%')
            || value.contains('?')
            || value.contains('#')
            || value.chars().any(char::is_control)
            || value
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        {
            return Err(CargoPathError::Invalid(value.to_owned()));
        }

        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CargoPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub fn normalize_crate_name(value: &str) -> Result<String, CargoPathError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || !value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(CargoPathError::InvalidCrateName(value.to_owned()));
    }

    Ok(value.to_ascii_lowercase())
}

pub(crate) fn validate_version(value: &str) -> Result<(), CargoPathError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_'))
    {
        return Err(CargoPathError::InvalidVersion(value.to_owned()));
    }
    Ok(())
}

pub(crate) fn validate_entry_name(value: &str) -> Result<(), CargoPathError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CargoPathError::Invalid(value.to_owned()));
    }
    Ok(())
}

pub fn sparse_index_path(crate_name: &str) -> Result<String, CargoPathError> {
    let crate_name = normalize_crate_name(crate_name)?;
    let bytes = crate_name.as_bytes();
    let path = match bytes.len() {
        1 => format!("1/{crate_name}"),
        2 => format!("2/{crate_name}"),
        3 => format!("3/{}/{crate_name}", &crate_name[..1]),
        _ => format!("{}/{}/{}", &crate_name[..2], &crate_name[2..4], crate_name),
    };
    Ok(path)
}
