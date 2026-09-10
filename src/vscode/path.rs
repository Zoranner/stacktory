use semver::Version;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VscodePath {
    Metadata {
        publisher: String,
        extension: String,
        version: Option<String>,
    },
    File {
        publisher: String,
        extension: String,
        version: String,
        filename: String,
    },
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum VscodePathError {
    #[error("VS Code path `{0}` is invalid")]
    Invalid(String),
}

impl VscodePath {
    pub fn parse(path: &str) -> Result<Self, VscodePathError> {
        let parts = path.split('/').collect::<Vec<_>>();
        if parts.first() != Some(&"api") || parts.iter().any(|part| !is_safe_segment(part)) {
            return Err(VscodePathError::Invalid(path.to_owned()));
        }

        match parts.as_slice() {
            ["api", publisher, extension] => Ok(Self::Metadata {
                publisher: (*publisher).to_owned(),
                extension: (*extension).to_owned(),
                version: None,
            }),
            ["api", publisher, extension, version] => {
                validate_version(version, path)?;
                Ok(Self::Metadata {
                    publisher: (*publisher).to_owned(),
                    extension: (*extension).to_owned(),
                    version: Some((*version).to_owned()),
                })
            }
            ["api", publisher, extension, version, "file", filename] => {
                validate_version(version, path)?;
                let expected = format!("{publisher}.{extension}-{version}.vsix");
                if *filename != expected {
                    return Err(VscodePathError::Invalid(path.to_owned()));
                }
                Ok(Self::File {
                    publisher: (*publisher).to_owned(),
                    extension: (*extension).to_owned(),
                    version: (*version).to_owned(),
                    filename: (*filename).to_owned(),
                })
            }
            _ => Err(VscodePathError::Invalid(path.to_owned())),
        }
    }

    pub(crate) fn metadata_key(&self) -> String {
        match self {
            Self::Metadata {
                publisher,
                extension,
                version,
            } => format!(
                "api/{publisher}/{extension}/{}",
                version.as_deref().unwrap_or("latest")
            ),
            Self::File {
                publisher,
                extension,
                version,
                ..
            } => format!("api/{publisher}/{extension}/{version}"),
        }
    }

    pub(crate) fn request_key(&self) -> String {
        match self {
            Self::Metadata {
                publisher,
                extension,
                version,
            } => match version {
                Some(version) => format!("api/{publisher}/{extension}/{version}"),
                None => format!("api/{publisher}/{extension}"),
            },
            Self::File {
                publisher,
                extension,
                version,
                filename,
            } => format!("api/{publisher}/{extension}/{version}/file/{filename}"),
        }
    }
}

fn validate_version(version: &str, path: &str) -> Result<(), VscodePathError> {
    Version::parse(version)
        .map(|_| ())
        .map_err(|_| VscodePathError::Invalid(path.to_owned()))
}

fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !segment.contains('%')
        && !segment.contains('\\')
        && !segment.chars().any(char::is_control)
        && segment != "."
        && segment != ".."
        && segment.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+' | b'~' | b'$')
        })
}
