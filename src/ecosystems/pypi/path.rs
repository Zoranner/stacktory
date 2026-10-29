use thiserror::Error;

const MAX_PROJECT_NAME_LENGTH: usize = 512;
const MAX_FILE_NAME_LENGTH: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectPath {
    pub(crate) project: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FilePath {
    pub(crate) project: String,
    pub(crate) filename: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PyPiPathError {
    #[error("PyPI path `{0}` is invalid")]
    Path(String),
    #[error("PyPI project name `{0}` is invalid")]
    ProjectName(String),
    #[error("PyPI file name `{0}` is invalid")]
    FileName(String),
}

/// PEP 503 项目名规范化：比较时用 `[-_.]+` 折叠为一个 `-` 并转小写。
pub(crate) fn normalize_project_name(value: &str) -> Result<String, PyPiPathError> {
    validate_project_component(value)?;
    let mut normalized = String::with_capacity(value.len());
    let mut previous_separator = false;
    for character in value.chars() {
        if matches!(character, '-' | '_' | '.') {
            if !previous_separator {
                normalized.push('-');
                previous_separator = true;
            }
        } else {
            normalized.push(character.to_ascii_lowercase());
            previous_separator = false;
        }
    }
    Ok(normalized)
}

/// `/simple/{project}/`：项目名必须恰好一段并以 `/` 结束。
pub(crate) fn parse_project_path(path: &str) -> Result<ProjectPath, PyPiPathError> {
    let rest = path
        .strip_prefix("simple/")
        .ok_or_else(|| PyPiPathError::Path(path.to_owned()))?;
    let rest = rest
        .strip_suffix('/')
        .ok_or_else(|| PyPiPathError::Path(path.to_owned()))?;
    if rest.is_empty()
        || rest.contains('/')
        || rest.contains('\\')
        || rest.contains('%')
        || rest.chars().any(char::is_control)
    {
        return Err(PyPiPathError::Path(path.to_owned()));
    }
    Ok(ProjectPath {
        project: normalize_project_name(rest)?,
    })
}

/// `/files/{project}/{filename}`：文件名允许一次百分号解码，但解码后仍是受限名称。
pub(crate) fn parse_file_path(path: &str) -> Result<FilePath, PyPiPathError> {
    let rest = path
        .strip_prefix("files/")
        .ok_or_else(|| PyPiPathError::Path(path.to_owned()))?;
    if rest.is_empty() || rest.ends_with('/') {
        return Err(PyPiPathError::Path(path.to_owned()));
    }
    let mut segments = rest.split('/');
    let project = segments.next().unwrap_or_default();
    let filename = segments.next().unwrap_or_default();
    if project.is_empty() || filename.is_empty() || segments.next().is_some() {
        return Err(PyPiPathError::Path(path.to_owned()));
    }
    let project = normalize_project_name(project)?;
    let filename = percent_decode(filename)?;
    if !is_safe_file_name(&filename) {
        return Err(PyPiPathError::FileName(filename));
    }
    Ok(FilePath { project, filename })
}

pub(crate) fn is_safe_file_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_FILE_NAME_LENGTH
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}

pub(crate) fn is_valid_entry_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(crate) fn percent_decode(value: &str) -> Result<String, PyPiPathError> {
    if value.is_empty() {
        return Err(PyPiPathError::Path(value.to_owned()));
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(PyPiPathError::Path(value.to_owned()));
            }
            let Some(high) = hex_value(bytes[index + 1]) else {
                return Err(PyPiPathError::Path(value.to_owned()));
            };
            let Some(low) = hex_value(bytes[index + 2]) else {
                return Err(PyPiPathError::Path(value.to_owned()));
            };
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| PyPiPathError::Path(value.to_owned()))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn validate_project_component(value: &str) -> Result<(), PyPiPathError> {
    let first = value.as_bytes().first().copied().unwrap_or_default();
    let last = value.as_bytes().last().copied().unwrap_or_default();
    if value.is_empty()
        || value.len() > MAX_PROJECT_NAME_LENGTH
        || matches!(value, "." | "..")
        || !first.is_ascii_alphanumeric()
        || !last.is_ascii_alphanumeric()
        || value.bytes().any(|byte| !byte.is_ascii())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(PyPiPathError::ProjectName(value.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{normalize_project_name, parse_file_path, parse_project_path};

    #[test]
    fn normalizes_aliases_to_the_same_project_key() {
        for alias in ["Friendly_Bard", "friendly-bard", "friendly.bard"] {
            assert_eq!(
                normalize_project_name(alias).expect("normalized name"),
                "friendly-bard",
                "{alias}"
            );
        }
        assert_eq!(
            normalize_project_name("a--b__c..d").expect("collapsed separators"),
            "a-b-c-d"
        );
        assert_eq!(
            normalize_project_name("0-._.-._.-._.-._.-._.-._.-0").expect("pep 503 name"),
            "0-0"
        );
    }

    #[test]
    fn rejects_invalid_project_components() {
        for value in [
            "", ".", "..", "-foo", "foo-", ".foo", "foo.", "_foo", "a/b", "a\\b", "%2e", "naïve",
            "a b",
        ] {
            assert!(normalize_project_name(value).is_err(), "{value}");
        }
    }

    #[test]
    fn parses_project_paths_with_a_required_trailing_slash() {
        let parsed = parse_project_path("simple/Friendly_Bard/").expect("project path");
        assert_eq!(parsed.project, "friendly-bard");
        for path in [
            "simple/friendly-bard",
            "simple/",
            "simple/friendly-bard/extra/",
            "simple/friendly-bard//",
            "simple/%2e%2e/",
            "simple/a%2fb/",
            "simple/a\\b/",
        ] {
            assert!(parse_project_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn parses_file_paths_and_decodes_percent_escapes() {
        let parsed = parse_file_path("files/Friendly_Bard/friendly_bard-1.0%2Bbuild.whl")
            .expect("file path");
        assert_eq!(parsed.project, "friendly-bard");
        assert_eq!(parsed.filename, "friendly_bard-1.0+build.whl");
    }

    #[test]
    fn rejects_encoded_traversal_and_absolute_file_paths() {
        for path in [
            "files/friendly-bard/%2e%2e%2fsecret",
            "files/friendly-bard/..%5csecret",
            "files/friendly-bard/https:%2F%2Fevil.example%2Fx.whl",
            "files/friendly-bard/",
            "files/friendly-bard/a/b.whl",
            "files/%2e%2e/a.whl",
        ] {
            assert!(parse_file_path(path).is_err(), "{path}");
        }
    }
}
