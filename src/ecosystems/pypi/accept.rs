use axum::http::{HeaderMap, header::ACCEPT};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PyPiRepresentation {
    Html,
    Json,
}

impl PyPiRepresentation {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Json => "json",
        }
    }

    pub(crate) const fn content_type(self) -> &'static str {
        match self {
            Self::Html => "text/html; charset=utf-8",
            Self::Json => "application/vnd.pypi.simple.v1+json",
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub(crate) enum AcceptError {
    #[error("PyPI Simple API representation is not acceptable")]
    NotAcceptable,
}

/// 未带 Accept 或只声明 `*/*` 固定返回 HTML；只有明确接受 v1 JSON 才返回 JSON。
pub(crate) fn negotiate(headers: &HeaderMap) -> Result<PyPiRepresentation, AcceptError> {
    let mut saw_non_empty = false;
    let mut json_quality = 0.0_f32;
    let mut html_quality = 0.0_f32;
    let mut wildcard_quality = 0.0_f32;

    for value in headers.get_all(ACCEPT) {
        let Ok(value) = value.to_str() else {
            return Err(AcceptError::NotAcceptable);
        };
        for media_range in value.split(',') {
            let media_range = media_range.trim();
            if media_range.is_empty() {
                continue;
            }
            saw_non_empty = true;
            let mut parts = media_range.split(';');
            let media_type = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
            let mut quality = 1.0_f32;
            for parameter in parts {
                let Some((name, value)) = parameter.trim().split_once('=') else {
                    continue;
                };
                if name.trim().eq_ignore_ascii_case("q") {
                    quality = value
                        .trim()
                        .trim_matches('"')
                        .parse::<f32>()
                        .ok()
                        .filter(|quality| (0.0..=1.0).contains(quality))
                        .unwrap_or(0.0);
                }
            }
            match media_type.as_str() {
                "application/vnd.pypi.simple.v1+json" => {
                    json_quality = json_quality.max(quality);
                }
                "text/html" | "application/vnd.pypi.simple.v1+html" => {
                    html_quality = html_quality.max(quality);
                }
                "*/*" => {
                    wildcard_quality = wildcard_quality.max(quality);
                }
                _ => {}
            }
        }
    }

    if !saw_non_empty {
        return Ok(PyPiRepresentation::Html);
    }

    let non_json_quality = html_quality.max(wildcard_quality);
    if json_quality > 0.0 && json_quality >= non_json_quality {
        return Ok(PyPiRepresentation::Json);
    }
    if html_quality > 0.0 || wildcard_quality > 0.0 {
        return Ok(PyPiRepresentation::Html);
    }
    Err(AcceptError::NotAcceptable)
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header::ACCEPT};

    use super::{AcceptError, PyPiRepresentation, negotiate};

    #[test]
    fn absent_and_wildcard_accept_default_to_html() {
        assert_eq!(negotiate(&headers(None)), Ok(PyPiRepresentation::Html));
        assert_eq!(
            negotiate(&headers(Some("*/*"))),
            Ok(PyPiRepresentation::Html)
        );
    }

    #[test]
    fn explicit_json_is_required_for_json() {
        assert_eq!(
            negotiate(&headers(Some("application/vnd.pypi.simple.v1+json"))),
            Ok(PyPiRepresentation::Json)
        );
        assert_eq!(
            negotiate(&headers(Some("application/json"))),
            Err(AcceptError::NotAcceptable)
        );
    }

    #[test]
    fn quality_values_choose_the_best_supported_representation() {
        assert_eq!(
            negotiate(&headers(Some(
                "application/vnd.pypi.simple.v1+json;q=0.5, text/html;q=0.9"
            ))),
            Ok(PyPiRepresentation::Html)
        );
        assert_eq!(
            negotiate(&headers(Some(
                "application/vnd.pypi.simple.v1+json, text/html;q=0.1"
            ))),
            Ok(PyPiRepresentation::Json)
        );
        assert_eq!(
            negotiate(&headers(Some(
                "application/vnd.pypi.simple.v1+json;q=0, text/html;q=0"
            ))),
            Err(AcceptError::NotAcceptable)
        );
    }

    #[test]
    fn multiple_accept_headers_are_combined() {
        let mut map = HeaderMap::new();
        map.append(ACCEPT, HeaderValue::from_static("text/html"));
        map.append(
            ACCEPT,
            HeaderValue::from_static("application/vnd.pypi.simple.v1+json;q=0.8"),
        );
        assert_eq!(negotiate(&map), Ok(PyPiRepresentation::Html));
    }

    fn headers(value: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(value) = value {
            headers.insert(ACCEPT, HeaderValue::from_str(value).expect("Accept header"));
        }
        headers
    }
}
