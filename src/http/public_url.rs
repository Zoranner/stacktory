//! 按请求头和配置的公开地址列表推导本次请求的绝对地址前缀。

use axum::http::HeaderMap;
use url::Url;

use crate::PublicUrls;

struct RequestHost {
    host: String,
    port: Option<u16>,
}

pub(crate) fn public_base_url_for_request(headers: &HeaderMap, public_urls: &PublicUrls) -> Url {
    let Some(host) =
        forwarded_header(headers, "x-forwarded-host").or_else(|| forwarded_header(headers, "host"))
    else {
        return public_urls.primary().clone();
    };
    let Some(request_host) = parse_request_host(host) else {
        return public_urls.primary().clone();
    };
    let scheme = match forwarded_header(headers, "x-forwarded-proto") {
        Some(value) if value.eq_ignore_ascii_case("http") => Some("http"),
        Some(value) if value.eq_ignore_ascii_case("https") => Some("https"),
        Some(_) => return public_urls.primary().clone(),
        None => None,
    };
    let mut matches = public_urls
        .iter()
        .filter(|candidate| candidate_matches(candidate, &request_host, scheme));
    let Some(first) = matches.next() else {
        return public_urls.primary().clone();
    };
    if matches.next().is_some() {
        return public_urls.primary().clone();
    }
    first.clone()
}

fn forwarded_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn parse_request_host(value: &str) -> Option<RequestHost> {
    if value.len() > 255 {
        return None;
    }
    let url = Url::parse(&format!("http://{value}")).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(RequestHost {
        host: url.host_str()?.to_ascii_lowercase(),
        port: url.port(),
    })
}

fn candidate_matches(candidate: &Url, request: &RequestHost, scheme: Option<&str>) -> bool {
    if scheme.is_some_and(|scheme| candidate.scheme() != scheme) {
        return false;
    }
    if !candidate
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case(&request.host))
    {
        return false;
    }
    let request_port = match (request.port, scheme) {
        (Some(port), _) => Some(port),
        (None, Some(scheme)) => default_port(scheme),
        (None, None) => default_port(candidate.scheme()),
    };
    request_port == candidate.port_or_known_default()
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}
