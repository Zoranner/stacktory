use std::fmt;

use super::RustupError;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DistPath(String);

impl DistPath {
    pub fn parse(value: &str) -> Result<Self, RustupError> {
        let invalid = value.is_empty()
            || value.starts_with('/')
            || value.contains('\\')
            || value.contains('%')
            || value
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."));

        if invalid {
            return Err(RustupError::InvalidDistPath(value.to_owned()));
        }

        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DistPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
