use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;
use url::Url;

use crate::upstream::pool::is_safe_configured_url_for_source;

use super::path::{PyPiPathError, is_safe_file_name, percent_decode};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PyPiIndex {
    pub(crate) name: String,
    pub(crate) files: Vec<PyPiFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PyPiFile {
    pub(crate) filename: String,
    pub(crate) upstream_url: String,
    #[serde(default)]
    pub(crate) hashes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requires_python: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) yanked: Option<String>,
}

#[derive(Debug, Error)]
pub enum PyPiIndexError {
    #[error("PyPI project index is not UTF-8")]
    InvalidUtf8,
    #[error("PyPI project index link `{0}` is invalid")]
    InvalidLink(String),
    #[error("PyPI project index link `{0}` is outside the configured source")]
    UnsafeLink(String),
    #[error("PyPI project index file name `{0}` is invalid")]
    InvalidFileName(String),
    #[error("PyPI project index hash `{0}` is invalid")]
    InvalidHash(String),
    #[error("PyPI project index contains duplicate file `{0}`")]
    DuplicateFile(String),
    #[error("PyPI project index returned an unexpected content type `{0}`")]
    UnexpectedContentType(String),
    #[error("PyPI project index serialization failed: {0}")]
    Serialization(String),
    #[error(transparent)]
    Path(#[from] PyPiPathError),
}

pub(crate) fn parse_project_index(
    body: &[u8],
    project_url: &Url,
    base_url: &Url,
    require_https: bool,
    project: &str,
) -> Result<PyPiIndex, PyPiIndexError> {
    let html = std::str::from_utf8(body).map_err(|_| PyPiIndexError::InvalidUtf8)?;
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();

    for tag in HtmlTags::new(html) {
        let Some((tag_name, attributes)) = parse_tag(tag) else {
            continue;
        };
        if !tag_name.eq_ignore_ascii_case("a") {
            continue;
        }
        let Some(href) = attributes.get("href").and_then(Option::as_deref) else {
            continue;
        };
        let href = decode_entities(href);
        let resolved = project_url
            .join(&href)
            .map_err(|_| PyPiIndexError::InvalidLink(href.clone()))?;
        let Some(filename) = file_name_from_url(&resolved)? else {
            continue;
        };
        let resolved = validate_download_url(resolved, base_url, require_https)?;
        let hashes = parse_hashes(resolved.fragment())?;
        if !seen.insert(filename.clone()) {
            return Err(PyPiIndexError::DuplicateFile(filename));
        }
        let mut upstream_url = resolved.clone();
        upstream_url.set_fragment(None);
        files.push(PyPiFile {
            filename,
            upstream_url: upstream_url.to_string(),
            hashes,
            requires_python: attributes
                .get("data-requires-python")
                .and_then(Option::as_deref)
                .map(decode_entities),
            yanked: attributes
                .get("data-yanked")
                .map(|value| value.as_deref().map(decode_entities).unwrap_or_default()),
        });
    }

    Ok(PyPiIndex {
        name: project.to_owned(),
        files,
    })
}

pub(crate) fn render_html<F>(index: &PyPiIndex, file_url: F) -> String
where
    F: Fn(&PyPiFile) -> String,
{
    let mut html = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n  <head>\n    \
         <meta name=\"pypi:repository-version\" content=\"1.0\">\n    \
         <title>Links for {}</title>\n  </head>\n  <body>\n    <h1>Links for {}</h1>\n",
        escape_html(&index.name),
        escape_html(&index.name)
    );
    for file in &index.files {
        html.push_str(&format!("<a href=\"{}\"", escape_html(&file_url(file))));
        if let Some(requires_python) = &file.requires_python {
            html.push_str(&format!(
                " data-requires-python=\"{}\"",
                escape_html(requires_python)
            ));
        }
        if let Some(yanked) = &file.yanked {
            if yanked.is_empty() {
                html.push_str(" data-yanked");
            } else {
                html.push_str(&format!(" data-yanked=\"{}\"", escape_html(yanked)));
            }
        }
        html.push_str(&format!(">{}</a><br />\n", escape_html(&file.filename)));
    }
    html.push_str("  </body>\n</html>\n");
    html
}

pub(crate) fn render_json<F>(index: &PyPiIndex, file_url: F) -> Result<Vec<u8>, serde_json::Error>
where
    F: Fn(&PyPiFile) -> String,
{
    let mut files = Vec::with_capacity(index.files.len());
    for file in &index.files {
        let mut object = Map::new();
        object.insert("filename".to_owned(), Value::String(file.filename.clone()));
        object.insert("url".to_owned(), Value::String(file_url(file)));
        object.insert(
            "hashes".to_owned(),
            Value::Object(
                file.hashes
                    .iter()
                    .map(|(algorithm, digest)| (algorithm.clone(), Value::String(digest.clone())))
                    .collect(),
            ),
        );
        if let Some(requires_python) = &file.requires_python {
            object.insert(
                "requires-python".to_owned(),
                Value::String(requires_python.clone()),
            );
        }
        if let Some(yanked) = &file.yanked {
            object.insert(
                "yanked".to_owned(),
                if yanked.is_empty() {
                    Value::Bool(true)
                } else {
                    Value::String(yanked.clone())
                },
            );
        }
        files.push(Value::Object(object));
    }

    serde_json::to_vec(&json!({
        "meta": {"api-version": "1.0"},
        "name": index.name,
        "files": files,
    }))
}

pub(crate) fn decode_entities(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        decoded.push_str(&rest[..index]);
        let candidate = &rest[index..];
        let Some(end) = candidate.find(';') else {
            decoded.push_str(candidate);
            return decoded;
        };
        if end > 12 {
            decoded.push('&');
            rest = &candidate[1..];
            continue;
        }
        let entity = &candidate[1..end];
        if let Some(character) = decode_entity(entity) {
            decoded.push(character);
            rest = &candidate[end + 1..];
        } else {
            decoded.push('&');
            rest = &candidate[1..];
        }
    }
    decoded.push_str(rest);
    decoded
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ if entity.starts_with("#x") || entity.starts_with("#X") => {
            u32::from_str_radix(&entity[2..], 16)
                .ok()
                .and_then(char::from_u32)
        }
        _ if entity.starts_with('#') => entity[1..].parse::<u32>().ok().and_then(char::from_u32),
        _ => None,
    }
}

fn file_name_from_url(url: &Url) -> Result<Option<String>, PyPiIndexError> {
    let Some(segment) = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
    else {
        return Ok(None);
    };
    if segment.is_empty() {
        return Ok(None);
    }
    let filename =
        percent_decode(segment).map_err(|_| PyPiIndexError::InvalidFileName(segment.to_owned()))?;
    if !is_safe_file_name(&filename) {
        return Err(PyPiIndexError::InvalidFileName(filename));
    }
    if !is_distribution_file(&filename) {
        return Ok(None);
    }
    Ok(Some(filename))
}

fn is_distribution_file(filename: &str) -> bool {
    let filename = filename.to_ascii_lowercase();
    [
        ".whl", ".tar.gz", ".tar.bz2", ".tar.xz", ".tgz", ".tbz2", ".zip", ".egg", ".tar",
    ]
    .iter()
    .any(|extension| filename.ends_with(extension))
}

fn validate_download_url(
    mut url: Url,
    base_url: &Url,
    require_https: bool,
) -> Result<Url, PyPiIndexError> {
    let original = url.to_string();
    let path = url.path().to_owned();
    let decoded_plus = path.replace("%2B", "+").replace("%2b", "+");
    if decoded_plus.contains('%') {
        return Err(PyPiIndexError::UnsafeLink(original));
    }
    if decoded_plus != path {
        url.set_path(&decoded_plus);
    }
    let mut without_fragment = url.clone();
    without_fragment.set_fragment(None);
    if !is_safe_configured_url_for_source(base_url, require_https, &without_fragment) {
        return Err(PyPiIndexError::UnsafeLink(original));
    }
    Ok(url)
}

fn parse_hashes(fragment: Option<&str>) -> Result<BTreeMap<String, String>, PyPiIndexError> {
    let Some(fragment) = fragment else {
        return Ok(BTreeMap::new());
    };
    let fragment =
        percent_decode(fragment).map_err(|_| PyPiIndexError::InvalidHash(fragment.to_owned()))?;
    let mut hashes = BTreeMap::new();
    for pair in fragment.split('&') {
        let Some((algorithm, digest)) = pair.split_once('=') else {
            continue;
        };
        let algorithm = algorithm.trim().to_ascii_lowercase();
        let expected_length = match algorithm.as_str() {
            "sha256" => 64,
            "sha384" => 96,
            "sha512" => 128,
            _ => continue,
        };
        let digest = digest.trim().to_ascii_lowercase();
        if digest.len() != expected_length || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(PyPiIndexError::InvalidHash(pair.to_owned()));
        }
        hashes.insert(algorithm, digest);
    }
    Ok(hashes)
}

pub(crate) fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            character => escaped.push(character),
        }
    }
    escaped
}

struct HtmlTags<'a> {
    rest: &'a str,
}

impl<'a> HtmlTags<'a> {
    fn new(html: &'a str) -> Self {
        Self { rest: html }
    }
}

impl<'a> Iterator for HtmlTags<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.rest.find('<')?;
        let bytes = self.rest.as_bytes();
        let mut index = start + 1;
        let mut quote = None;
        while index < bytes.len() {
            let byte = bytes[index];
            if let Some(active_quote) = quote {
                if byte == active_quote {
                    quote = None;
                }
            } else if matches!(byte, b'"' | b'\'') {
                quote = Some(byte);
            } else if byte == b'>' {
                let tag = &self.rest[start..=index];
                self.rest = &self.rest[index + 1..];
                return Some(tag);
            }
            index += 1;
        }
        self.rest = "";
        None
    }
}

fn parse_tag(tag: &str) -> Option<(&str, BTreeMap<String, Option<String>>)> {
    let inner = tag.strip_prefix('<')?.trim_start();
    if inner.starts_with('/') || inner.starts_with('!') || inner.starts_with('?') {
        return None;
    }
    let mut index = 0;
    let bytes = inner.as_bytes();
    while index < bytes.len() && !bytes[index].is_ascii_whitespace() && bytes[index] != b'>' {
        index += 1;
    }
    let name = &inner[..index];
    let mut attributes = BTreeMap::new();
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b'/') {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] == b'>' {
            break;
        }
        let name_start = index;
        while index < bytes.len()
            && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'-' | b'_' | b':'))
        {
            index += 1;
        }
        if name_start == index {
            index += 1;
            continue;
        }
        let attribute_name = inner[name_start..index].to_ascii_lowercase();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let mut value = Some(String::new());
        if index < bytes.len() && bytes[index] == b'=' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index < bytes.len() && matches!(bytes[index], b'"' | b'\'') {
                let quote = bytes[index];
                index += 1;
                let value_start = index;
                while index < bytes.len() && bytes[index] != quote {
                    index += 1;
                }
                value = Some(decode_entities(&inner[value_start..index]));
                if index < bytes.len() {
                    index += 1;
                }
            } else {
                let value_start = index;
                while index < bytes.len()
                    && !bytes[index].is_ascii_whitespace()
                    && bytes[index] != b'>'
                {
                    index += 1;
                }
                value = Some(decode_entities(&inner[value_start..index]));
            }
        }
        attributes.entry(attribute_name).or_insert(value);
    }
    Some((name, attributes))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use url::Url;

    use super::{parse_project_index, render_html, render_json};
    use crate::ecosystems::pypi::path::normalize_project_name;

    #[test]
    fn parses_file_links_hashes_and_python_requirements() {
        let base = Url::parse("https://pypi.example").expect("base URL");
        let project_url =
            Url::parse("https://pypi.example/simple/friendly-bard/").expect("project URL");
        let html = br#"
            <html><body>
            <a href="/packages/friendly_bard-1.0.0-py3-none-any.whl#sha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" data-requires-python="&gt;=3.8" data-core-metadata="sha256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb">friendly_bard-1.0.0-py3-none-any.whl</a>
            <a href="../friendly-bard/">navigation</a>
            </body></html>
        "#;
        let project = normalize_project_name("friendly-bard").expect("normalized name");
        let index =
            parse_project_index(html, &project_url, &base, true, &project).expect("project index");
        assert_eq!(index.files.len(), 1);
        let file = &index.files[0];
        assert_eq!(file.filename, "friendly_bard-1.0.0-py3-none-any.whl");
        assert_eq!(file.requires_python.as_deref(), Some(">=3.8"));
        assert_eq!(
            file.hashes["sha256"],
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(
            file.upstream_url
                .starts_with("https://pypi.example/packages/")
        );
    }

    #[test]
    fn rejects_encoded_file_name_traversal() {
        let base = Url::parse("https://pypi.example").expect("base URL");
        let project_url =
            Url::parse("https://pypi.example/simple/friendly-bard/").expect("project URL");
        let html = br#"<a href="/packages/%2e%2e%2fsecret.whl">secret.whl</a>"#;
        let error = parse_project_index(html, &project_url, &base, true, "friendly-bard")
            .expect_err("traversal must fail");
        assert!(error.to_string().contains("invalid"), "{error}");
    }

    #[test]
    fn renders_json_with_protocol_field_names() {
        let base = Url::parse("https://pypi.example").expect("base URL");
        let project_url =
            Url::parse("https://pypi.example/simple/friendly-bard/").expect("project URL");
        let html = br#"<a href="/packages/friendly_bard-1.0.0-py3-none-any.whl#sha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" data-requires-python="&gt;=3.8" data-yanked="bad release" data-core-metadata="sha256=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb">friendly_bard-1.0.0-py3-none-any.whl</a>"#;
        let index = parse_project_index(html, &project_url, &base, true, "friendly-bard")
            .expect("project index");
        let json = render_json(&index, |file| {
            format!("https://mirror.example/{}", file.filename)
        })
        .expect("JSON");
        let value: serde_json::Value = serde_json::from_slice(&json).expect("JSON value");
        assert_eq!(value["name"], "friendly-bard");
        assert_eq!(value["files"][0]["requires-python"], ">=3.8");
        assert_eq!(value["files"][0]["yanked"], "bad release");
        assert!(value["files"][0].get("core-metadata").is_none());
        assert_eq!(
            value["files"][0]["hashes"],
            serde_json::json!({"sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})
        );
    }

    #[test]
    fn renders_html_with_escaped_attributes() {
        let base = Url::parse("https://pypi.example").expect("base URL");
        let project_url =
            Url::parse("https://pypi.example/simple/friendly-bard/").expect("project URL");
        let html = br#"<a href="/packages/friendly_bard-1.0.0-py3-none-any.whl" data-requires-python="&gt;=3.8">friendly_bard-1.0.0-py3-none-any.whl</a>"#;
        let index = parse_project_index(html, &project_url, &base, true, "friendly-bard")
            .expect("project index");
        let html = render_html(&index, |file| {
            format!("https://mirror.example/{}", file.filename)
        });
        assert!(html.contains("&gt;=3.8"), "{html}");
        assert!(
            html.contains("href=\"https://mirror.example/friendly_bard-1.0.0-py3-none-any.whl\"")
        );
    }

    #[test]
    fn empty_hash_map_stays_present_in_json() {
        let base = Url::parse("https://pypi.example").expect("base URL");
        let project_url =
            Url::parse("https://pypi.example/simple/friendly-bard/").expect("project URL");
        let html = br#"<a href="/packages/friendly_bard-1.0.0-py3-none-any.whl">friendly_bard-1.0.0-py3-none-any.whl</a>"#;
        let index = parse_project_index(html, &project_url, &base, true, "friendly-bard")
            .expect("project index");
        let json = render_json(&index, |file| {
            format!("https://mirror.example/{}", file.filename)
        })
        .expect("JSON");
        let value: serde_json::Value = serde_json::from_slice(&json).expect("JSON value");
        assert_eq!(value["files"][0]["hashes"], serde_json::json!({}));
        let _: BTreeMap<String, String> = BTreeMap::new();
    }
}
