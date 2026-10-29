//! `/simple/` 根索引：流式转发，HTML 项目链接改写为当前入口。

use std::io;

use axum::{
    body::{Body, Bytes},
    http::{
        HeaderValue, StatusCode,
        header::{CONTENT_TYPE, VARY},
    },
    response::{IntoResponse, Response},
};
use futures_util::stream;
use url::Url;

use crate::{
    SourceConfig,
    upstream::{
        FetchOperation, GroupRun, SourcePoolError, pool::SourceStream,
        source_error_is_group_fallback, source_error_is_not_found,
    },
};

use super::{
    PyPiError, html_headers, internal_root_prefix, is_html_content_type, is_json_content_type,
    json_headers, upstream_simple_url,
};
use crate::ecosystems::pypi::{
    accept::PyPiRepresentation,
    index::{decode_entities, escape_html},
    path::normalize_project_name,
};

const MAX_HTML_TAG_BYTES: usize = 64 * 1024;

impl super::PyPiService {
    pub(super) async fn root_index(
        &self,
        entry_name: &str,
        representation: PyPiRepresentation,
        pool: &crate::SourcePool,
        base_url: &Url,
    ) -> Response {
        let (source, member) = match self.root_source(entry_name, representation, pool).await {
            Ok(source) => source,
            Err(error) => return self.error_response(&error),
        };
        self.record(
            "pypi_root_index_read",
            entry_name,
            "simple/",
            Some(&member.name),
            "miss",
            None,
        );

        let rewriter = match representation {
            PyPiRepresentation::Html => member.base_url.as_ref().map(|source_base_url| {
                HtmlLinkRewriter::new(base_url, source_base_url, entry_name)
            }),
            PyPiRepresentation::Json => None,
        };
        let body = Body::from_stream(root_body_stream(source, rewriter));
        let mut response = (
            StatusCode::OK,
            [(CONTENT_TYPE, representation.content_type())],
            body,
        )
            .into_response();
        response
            .headers_mut()
            .insert(VARY, HeaderValue::from_static("Accept"));
        response
    }

    async fn root_source(
        &self,
        entry_name: &str,
        representation: PyPiRepresentation,
        pool: &crate::SourcePool,
    ) -> Result<(SourceStream, SourceConfig), PyPiError> {
        let mut run = GroupRun::new(pool, entry_name, None)?;
        while let Some(member) = run.next_member() {
            let headers = match representation {
                PyPiRepresentation::Html => html_headers(),
                PyPiRepresentation::Json => json_headers(),
            };
            let source = match pool
                .fetch_proxy_path_stream(&member.name, "simple/", headers, FetchOperation::Metadata)
                .await
            {
                Ok(source) => source,
                Err(error) if run.is_group() && source_error_is_group_fallback(&error) => {
                    run.record_failure(
                        &member.name,
                        &error.to_string(),
                        source_error_is_not_found(&error),
                    );
                    self.record(
                        "pypi_root_index_source_failed",
                        entry_name,
                        "simple/",
                        Some(&member.name),
                        "failed",
                        Some(source_failure_category(&error)),
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let content_type_valid = match representation {
                PyPiRepresentation::Html => is_html_content_type(source.content_type.as_deref()),
                PyPiRepresentation::Json => is_json_content_type(source.content_type.as_deref()),
            };
            if !content_type_valid {
                run.record_failure(&member.name, "unexpected root index content type", false);
                self.record(
                    "pypi_root_index_source_failed",
                    entry_name,
                    "simple/",
                    Some(&member.name),
                    "failed",
                    Some("protocol_invalid"),
                );
                continue;
            }
            return Ok((source, member));
        }

        let error = run.all_sources_failed("simple/");
        self.record(
            "pypi_root_index_fetch_failed",
            entry_name,
            "simple/",
            None,
            "failed",
            Some(source_failure_category(&error)),
        );
        Err(PyPiError::Source(error))
    }
}

fn source_failure_category(error: &SourcePoolError) -> &'static str {
    if source_error_is_not_found(error) {
        "not_found"
    } else {
        "source_failed"
    }
}

fn root_body_stream(
    source: SourceStream,
    rewriter: Option<HtmlLinkRewriter>,
) -> impl futures_util::Stream<Item = Result<Bytes, io::Error>> + Send {
    stream::unfold(
        (source, rewriter, false),
        |(mut source, mut rewriter, finished)| async move {
            if finished {
                return None;
            }
            match source.next_chunk().await {
                Ok(Some(chunk)) => {
                    let output = rewriter
                        .as_mut()
                        .map(|rewriter| rewriter.push(&chunk))
                        .unwrap_or(chunk);
                    Some((Ok(Bytes::from(output)), (source, rewriter, false)))
                }
                Ok(None) => {
                    let output = rewriter
                        .as_mut()
                        .map(HtmlLinkRewriter::finish)
                        .unwrap_or_default();
                    if output.is_empty() {
                        None
                    } else {
                        Some((Ok(Bytes::from(output)), (source, rewriter, true)))
                    }
                }
                Err(error) => Some((
                    Err(io::Error::other(error.to_string())),
                    (source, rewriter, true),
                )),
            }
        },
    )
}

struct HtmlLinkRewriter {
    upstream_simple: Url,
    upstream_prefix: String,
    internal_prefix: String,
    pending: Vec<u8>,
}

impl HtmlLinkRewriter {
    fn new(public_base_url: &Url, source_base_url: &Url, entry_name: &str) -> Self {
        let upstream_simple = upstream_simple_url(source_base_url);
        let upstream_prefix = upstream_simple.path().to_owned();
        Self {
            upstream_simple,
            upstream_prefix,
            internal_prefix: internal_root_prefix(public_base_url, entry_name),
            pending: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let mut output = Vec::with_capacity(self.pending.len());
        let mut consumed = 0;
        loop {
            let Some(relative_start) = self.pending[consumed..]
                .iter()
                .position(|byte| *byte == b'<')
            else {
                output.extend_from_slice(&self.pending[consumed..]);
                consumed = self.pending.len();
                break;
            };
            let start = consumed + relative_start;
            output.extend_from_slice(&self.pending[consumed..start]);
            let Some(tag_length) = tag_length(&self.pending[start..]) else {
                if self.pending.len() - start > MAX_HTML_TAG_BYTES {
                    output.extend_from_slice(&self.pending[start..]);
                    consumed = self.pending.len();
                } else {
                    consumed = start;
                }
                break;
            };
            let end = start + tag_length;
            output.extend_from_slice(&self.rewrite_tag(&self.pending[start..end]));
            consumed = end;
        }
        self.pending.drain(..consumed);
        output
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }

    fn rewrite_tag(&self, tag: &[u8]) -> Vec<u8> {
        let Some(attribute_start) = anchor_name_end(tag) else {
            return tag.to_vec();
        };
        let Some((value_start, value_end)) = href_value(tag, attribute_start) else {
            return tag.to_vec();
        };
        let Ok(raw_href) = std::str::from_utf8(&tag[value_start..value_end]) else {
            return tag.to_vec();
        };
        let href = decode_entities(raw_href);
        let Ok(resolved) = self.upstream_simple.join(&href) else {
            return tag.to_vec();
        };
        let Some(rest) = resolved.path().strip_prefix(&self.upstream_prefix) else {
            return tag.to_vec();
        };
        let Some(rest) = rest.strip_suffix('/') else {
            return tag.to_vec();
        };
        if rest.is_empty() || rest.contains('/') {
            return tag.to_vec();
        }
        let Ok(project) = normalize_project_name(rest) else {
            return tag.to_vec();
        };
        let internal = escape_html(&format!("{}{project}/", self.internal_prefix));

        let mut output = Vec::with_capacity(tag.len() + internal.len());
        output.extend_from_slice(&tag[..value_start]);
        output.extend_from_slice(internal.as_bytes());
        output.extend_from_slice(&tag[value_end..]);
        output
    }
}

fn tag_length(input: &[u8]) -> Option<usize> {
    let mut quote = None;
    for (index, byte) in input.iter().copied().enumerate().skip(1) {
        if let Some(active_quote) = quote {
            if byte == active_quote {
                quote = None;
            }
        } else if matches!(byte, b'"' | b'\'') {
            quote = Some(byte);
        } else if byte == b'>' {
            return Some(index + 1);
        }
    }
    None
}

fn anchor_name_end(tag: &[u8]) -> Option<usize> {
    let mut index = 1;
    while index < tag.len() && tag[index].is_ascii_whitespace() {
        index += 1;
    }
    if index >= tag.len() || !matches!(tag[index], b'a' | b'A') {
        return None;
    }
    index += 1;
    if index < tag.len() && !tag[index].is_ascii_whitespace() && !matches!(tag[index], b'>' | b'/')
    {
        return None;
    }
    Some(index)
}

fn href_value(tag: &[u8], mut index: usize) -> Option<(usize, usize)> {
    while index < tag.len() {
        while index < tag.len() && (tag[index].is_ascii_whitespace() || tag[index] == b'/') {
            index += 1;
        }
        if index >= tag.len() || tag[index] == b'>' {
            return None;
        }
        let name_start = index;
        while index < tag.len()
            && !tag[index].is_ascii_whitespace()
            && !matches!(tag[index], b'=' | b'>' | b'/')
        {
            index += 1;
        }
        if name_start == index {
            index += 1;
            continue;
        }
        let is_href = tag[name_start..index].eq_ignore_ascii_case(b"href");
        while index < tag.len() && tag[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= tag.len() || tag[index] != b'=' {
            if is_href {
                return None;
            }
            continue;
        }
        index += 1;
        while index < tag.len() && tag[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= tag.len() {
            return None;
        }
        if matches!(tag[index], b'"' | b'\'') {
            let quote = tag[index];
            index += 1;
            let value_start = index;
            while index < tag.len() && tag[index] != quote {
                index += 1;
            }
            if index >= tag.len() {
                return None;
            }
            if is_href {
                return Some((value_start, index));
            }
            index += 1;
        } else {
            let value_start = index;
            while index < tag.len() && !tag[index].is_ascii_whitespace() && tag[index] != b'>' {
                index += 1;
            }
            if is_href {
                return Some((value_start, index));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::HtmlLinkRewriter;

    #[test]
    fn rewrites_project_links_split_across_chunks() {
        let public = Url::parse("https://mirror.example/artifacts").expect("public URL");
        let source = Url::parse("https://pypi.example").expect("source URL");
        let mut rewriter = HtmlLinkRewriter::new(&public, &source, "group");

        let mut output = rewriter.push(b"<html><body><a hr");
        output.extend(rewriter.push(b"ef=\"/simple/Friendly_Bard/\">Friendly Bard</a>"));
        output.extend(rewriter.finish());
        let html = String::from_utf8(output).expect("UTF-8");

        assert!(html.contains("href=\"/artifacts/mirror/pypi/group/simple/friendly-bard/\""));
    }

    #[test]
    fn leaves_non_project_links_unchanged() {
        let public = Url::parse("https://mirror.example").expect("public URL");
        let source = Url::parse("https://pypi.example").expect("source URL");
        let mut rewriter = HtmlLinkRewriter::new(&public, &source, "group");

        let tag = b"<a href=\"https://example.com/elsewhere\">elsewhere</a>";
        let output = rewriter.push(tag);

        assert_eq!(output, tag);
    }
}
