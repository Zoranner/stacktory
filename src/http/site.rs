//! 首页和内嵌静态资源。
//!
//! 站点文件编译进二进制，不依赖进程运行目录。

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use url::Url;

use super::{html_response, public_base_url_for_request, text_response};
use crate::config::{PublicUrls, SourceCatalog};

#[derive(Debug)]
pub(crate) struct SiteContext {
    public_urls: PublicUrls,
    catalog: SourceCatalog,
}

impl SiteContext {
    pub(crate) fn new(public_urls: PublicUrls, catalog: SourceCatalog) -> Self {
        Self {
            public_urls,
            catalog,
        }
    }

    pub(crate) fn public_urls(&self) -> &PublicUrls {
        &self.public_urls
    }

    pub(crate) fn catalog(&self) -> &SourceCatalog {
        &self.catalog
    }
}

const LANDING_HTML: &str = include_str!("../../web/index.html");
const LANDING_CSS: &str = include_str!("../../web/index.css");
const LANDING_JS: &str = include_str!("../../web/index.js");
const THEME_CSS: &str = include_str!("../../web/theme.css");
const VENDOR_MODULES: &[(&str, &str)] = &[
    (
        "three.min.js",
        include_str!("../../web/vendor/three.min.js"),
    ),
    (
        "three.module.js",
        include_str!("../../web/vendor/three.module.js"),
    ),
    (
        "three-addons/postprocessing/EffectComposer.js",
        include_str!("../../web/vendor/three-addons/postprocessing/EffectComposer.js"),
    ),
    (
        "three-addons/postprocessing/RenderPass.js",
        include_str!("../../web/vendor/three-addons/postprocessing/RenderPass.js"),
    ),
    (
        "three-addons/postprocessing/UnrealBloomPass.js",
        include_str!("../../web/vendor/three-addons/postprocessing/UnrealBloomPass.js"),
    ),
    (
        "three-addons/postprocessing/OutputPass.js",
        include_str!("../../web/vendor/three-addons/postprocessing/OutputPass.js"),
    ),
    (
        "three-addons/postprocessing/Pass.js",
        include_str!("../../web/vendor/three-addons/postprocessing/Pass.js"),
    ),
    (
        "three-addons/postprocessing/ShaderPass.js",
        include_str!("../../web/vendor/three-addons/postprocessing/ShaderPass.js"),
    ),
    (
        "three-addons/postprocessing/MaskPass.js",
        include_str!("../../web/vendor/three-addons/postprocessing/MaskPass.js"),
    ),
    (
        "three-addons/shaders/CopyShader.js",
        include_str!("../../web/vendor/three-addons/shaders/CopyShader.js"),
    ),
    (
        "three-addons/shaders/LuminosityHighPassShader.js",
        include_str!("../../web/vendor/three-addons/shaders/LuminosityHighPassShader.js"),
    ),
    (
        "three-addons/shaders/OutputShader.js",
        include_str!("../../web/vendor/three-addons/shaders/OutputShader.js"),
    ),
];

pub(super) async fn landing_page(
    State(site): State<Arc<SiteContext>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let base_url = public_base_url_for_request(&headers, site.public_urls());
    let home = site_href(&base_url, "/");
    let docs = site_href(&base_url, "/docs");
    let css = site_href(&base_url, "/index.css");
    let theme = site_href(&base_url, "/theme.css");
    let js = site_href(&base_url, "/index.js");
    let three = site_href(&base_url, "/vendor/three.module.js");
    let three_addons = site_href(&base_url, "/vendor/three-addons/");
    let html = LANDING_HTML
        .replace("{{HOME}}", &home)
        .replace("{{DOCS}}", &docs)
        .replace("{{CSS}}", &css)
        .replace("{{THEME}}", &theme)
        .replace("{{JS}}", &js)
        .replace("{{THREE}}", &three)
        .replace("{{THREE_ADDONS}}", &three_addons)
        .replace("{{SERVICE_URL}}", base_url.as_str());
    html_response(html)
}

pub(super) async fn landing_css() -> axum::response::Response {
    asset_response("text/css; charset=utf-8", LANDING_CSS)
}

pub(super) async fn theme_css() -> axum::response::Response {
    text_response("text/css; charset=utf-8", THEME_CSS.to_owned())
}

pub(super) async fn landing_js() -> axum::response::Response {
    asset_response("text/javascript; charset=utf-8", LANDING_JS)
}

pub(super) async fn vendor_module(Path(path): Path<String>) -> axum::response::Response {
    let Some((_, body)) = VENDOR_MODULES
        .iter()
        .find(|(name, _)| *name == path.as_str())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    asset_response("text/javascript; charset=utf-8", body)
}

fn asset_response(content_type: &'static str, body: &'static str) -> axum::response::Response {
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(content_type),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("public, max-age=3600"),
    );
    response
}

fn site_href(base_url: &Url, suffix: &str) -> String {
    format!(
        "{}{suffix}",
        base_url.path().trim_end_matches('/').to_owned()
    )
}
