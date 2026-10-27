//! `/docs` 使用指南页面。
//!
//! 页面内容来自同一模块的渲染器，这里只负责把请求头推导出的公开地址传进去。

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};

use super::super::site::SiteContext;
use super::super::{html_response, public_base_url_for_request, text_response};
use super::{
    render_index_html, render_index_markdown, render_section_html, render_section_markdown,
    section_by_id,
};

const STYLESHEET: &str = include_str!("../../../web/guide/index.css");
const SCRIPT: &str = include_str!("../../../web/guide/index.js");

pub(crate) async fn stylesheet() -> axum::response::Response {
    text_response("text/css; charset=utf-8", STYLESHEET.to_owned())
}

pub(crate) async fn script() -> axum::response::Response {
    text_response("text/javascript; charset=utf-8", SCRIPT.to_owned())
}

pub(crate) async fn index_page(
    State(site): State<Arc<SiteContext>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let base_url = public_base_url_for_request(&headers, site.public_urls());
    html_response(render_index_html(base_url.as_str(), site.catalog()))
}

pub(crate) async fn section_page(
    State(site): State<Arc<SiteContext>>,
    Path(stack): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    let Some(section) = section_by_id(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let base_url = public_base_url_for_request(&headers, site.public_urls());
    html_response(render_section_html(
        section,
        base_url.as_str(),
        site.catalog(),
    ))
}

pub(crate) async fn index_markdown(
    State(site): State<Arc<SiteContext>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let base_url = public_base_url_for_request(&headers, site.public_urls());
    text_response(
        "text/markdown; charset=utf-8",
        render_index_markdown(base_url.as_str(), site.catalog()),
    )
}

pub(crate) async fn section_markdown(
    State(site): State<Arc<SiteContext>>,
    Path(stack): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    let Some(section) = section_by_id(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let base_url = public_base_url_for_request(&headers, site.public_urls());
    text_response(
        "text/markdown; charset=utf-8",
        render_section_markdown(section, base_url.as_str(), site.catalog()),
    )
}
